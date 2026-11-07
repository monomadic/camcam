import CryptoKit
import Foundation

private let media1 = "http://www.onvif.org/ver10/media/wsdl"
private let media2 = "http://www.onvif.org/ver20/media/wsdl"
private let deviceService = "http://www.onvif.org/ver10/device/wsdl"
private let security = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss"

struct ONVIF {
    let device: URL
    let credentials: Credentials?

    func streams() async throws -> Streams {
        let services: XMLNode
        do {
            services = try await request(
                device, deviceService, "GetServices", "<m:IncludeCapability>false</m:IncludeCapability>"
            )
        } catch {
            try Task.checkCancellation()
            let capabilities = try await request(
                device, deviceService, "GetCapabilities", "<m:Category>Media</m:Category>"
            )
            guard let address = capabilities.first("Media")?.child("XAddr")?.value,
                  let endpoint = serviceURL(address) else {
                throw CameraError("ONVIF camera returned no media service")
            }
            return try await streams(endpoint, media1)
        }

        var lastError: Error = CameraError("ONVIF camera returned no media service")
        for namespace in [media2, media1] {
            let service = services.all("Service").first { $0.child("Namespace")?.value == namespace }
            guard let address = service?.child("XAddr")?.value,
                  let endpoint = serviceURL(address) else { continue }
            do {
                return try await streams(endpoint, namespace)
            } catch {
                try Task.checkCancellation()
                lastError = error
            }
        }
        throw lastError
    }

    private func serviceURL(_ address: String) -> URL? {
        guard let url = URL(string: address, relativeTo: device)?.absoluteURL,
              url.host?.lowercased() == device.host?.lowercased(),
              ["http", "https"].contains(url.scheme ?? ""),
              device.scheme != "https" || url.scheme == "https" else { return nil }
        return url
    }

    private func streams(_ endpoint: URL, _ namespace: String) async throws -> Streams {
        let type = namespace == media2 ? "<m:Type>VideoEncoder</m:Type>" : ""
        let response = try await request(endpoint, namespace, "GetProfiles", type)
        let profiles = response.all("Profiles").compactMap { node -> (token: String, pixels: Double)? in
            guard let token = node.attributes["token"] else { return nil }
            if let encoding = node.first("Encoding")?.value.uppercased(),
               !["H264", "H265", "HEVC"].contains(encoding) { return nil }
            let resolution = node.first("Resolution")
            let width = Double(resolution?.child("Width")?.value ?? "") ?? 0
            let height = Double(resolution?.child("Height")?.value ?? "") ?? 0
            return (token, width * height)
        }
        guard let first = profiles.first else { throw CameraError("ONVIF camera returned no H.264/HEVC media profiles") }
        let sized = profiles.filter { $0.pixels > 0 }
        let small = sized.min { $0.pixels < $1.pixels } ?? first
        let large = sized.max { $0.pixels < $1.pixels } ?? first

        let wall: URL
        do {
            wall = try await streamURL(endpoint, namespace, token: small.token)
        } catch {
            try Task.checkCancellation()
            guard large.token != small.token else { throw error }
            return Streams(wall: try await streamURL(endpoint, namespace, token: large.token))
        }
        guard large.token != small.token, large.pixels > small.pixels else { return Streams(wall: wall) }
        let focus = try? await streamURL(endpoint, namespace, token: large.token)
        try Task.checkCancellation()
        return Streams(wall: wall, focus: focus == wall ? nil : focus)
    }

    private func streamURL(_ endpoint: URL, _ namespace: String, token: String) async throws -> URL {
        let setup = namespace == media2
            ? "<m:Protocol>RTSP</m:Protocol>"
            : """
            <m:StreamSetup>
              <t:Stream>RTP-Unicast</t:Stream>
              <t:Transport><t:Protocol>RTSP</t:Protocol></t:Transport>
            </m:StreamSetup>
            """
        let response = try await request(
            endpoint, namespace, "GetStreamUri",
            "\(setup)<m:ProfileToken>\(xmlEscape(token))</m:ProfileToken>"
        )
        guard let uri = response.first("Uri")?.value,
              let url = URL(string: uri), url.scheme == "rtsp", url.host != nil else {
            throw CameraError("ONVIF camera returned no usable RTSP URL")
        }
        if url.host?.lowercased() == device.host?.lowercased(), let credentials {
            return credentials.apply(to: url)
        }
        return url
    }

    private func request(_ endpoint: URL, _ namespace: String, _ action: String, _ body: String) async throws -> XMLNode {
        var request = URLRequest(url: endpoint, timeoutInterval: 5)
        request.httpMethod = "POST"
        request.setValue("application/soap+xml; charset=utf-8; action=\"\(namespace)/\(action)\"",
                         forHTTPHeaderField: "Content-Type")
        request.httpBody = Data("""
        <s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:m="\(namespace)"
                    xmlns:t="http://www.onvif.org/ver10/schema">
          <s:Header>\(securityHeader())</s:Header>
          <s:Body><m:\(action)>\(body)</m:\(action)></s:Body>
        </s:Envelope>
        """.utf8)

        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForResource = 6
        let session = URLSession(configuration: configuration)
        defer { session.finishTasksAndInvalidate() }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request, delegate: SOAPDelegate(credentials))
        } catch {
            try Task.checkCancellation()
            throw CameraError("ONVIF \(action): \((error as NSError).code == NSURLErrorTimedOut ? "timed out" : "connection failed")")
        }
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw CameraError("ONVIF \(action): HTTP \((response as? HTTPURLResponse)?.statusCode ?? 0)")
        }

        let tree = SOAPParser()
        let parser = XMLParser(data: data)
        parser.shouldProcessNamespaces = true
        parser.delegate = tree
        guard parser.parse(), let body = tree.root.first("Body"), body.child("Fault") == nil else {
            throw CameraError("ONVIF \(action): invalid response or SOAP fault")
        }
        return body
    }

    private func securityHeader() -> String {
        guard let credentials else { return "" }
        let nonce = Data((0..<20).map { _ in UInt8.random(in: .min ... .max) })
        let created = ISO8601DateFormatter().string(from: Date())
        let digest = Insecure.SHA1.hash(data: nonce + Data((created + credentials.password).utf8))
        return """
        <wsse:Security xmlns:wsse="\(security)-wssecurity-secext-1.0.xsd"
                       xmlns:wsu="\(security)-wssecurity-utility-1.0.xsd">
          <wsse:UsernameToken>
            <wsse:Username>\(xmlEscape(credentials.user))</wsse:Username>
            <wsse:Password Type="\(security)-username-token-profile-1.0#PasswordDigest">\(Data(digest).base64EncodedString())</wsse:Password>
            <wsse:Nonce EncodingType="\(security)-soap-message-security-1.0#Base64Binary">\(nonce.base64EncodedString())</wsse:Nonce>
            <wsu:Created>\(created)</wsu:Created>
          </wsse:UsernameToken>
        </wsse:Security>
        """
    }
}

private final class SOAPDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    let credentials: Credentials?
    init(_ credentials: Credentials?) { self.credentials = credentials }

    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest) async -> URLRequest? { nil }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    didReceive challenge: URLAuthenticationChallenge) async -> (URLSession.AuthChallengeDisposition, URLCredential?) {
        let space = challenge.protectionSpace
        guard space.authenticationMethod == NSURLAuthenticationMethodHTTPDigest ||
              (space.authenticationMethod == NSURLAuthenticationMethodHTTPBasic && space.receivesCredentialSecurely) else {
            return (.performDefaultHandling, nil)
        }
        guard let credentials, challenge.previousFailureCount == 0 else { return (.cancelAuthenticationChallenge, nil) }
        return (.useCredential, URLCredential(user: credentials.user, password: credentials.password, persistence: .none))
    }
}

private func xmlEscape(_ value: String) -> String {
    value.replacingOccurrences(of: "&", with: "&amp;")
        .replacingOccurrences(of: "<", with: "&lt;")
        .replacingOccurrences(of: ">", with: "&gt;")
}

private final class XMLNode {
    let name: String
    let attributes: [String: String]
    var text = ""
    var children: [XMLNode] = []
    var value: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    init(_ name: String, attributes: [String: String] = [:]) {
        self.name = name
        self.attributes = attributes
    }

    func child(_ name: String) -> XMLNode? { children.first { $0.name == name } }
    func first(_ name: String) -> XMLNode? {
        if self.name == name { return self }
        for child in children {
            if let match = child.first(name) { return match }
        }
        return nil
    }
    func all(_ name: String) -> [XMLNode] {
        if self.name == name { return [self] }
        return children.flatMap { $0.all(name) }
    }
}

private final class SOAPParser: NSObject, XMLParserDelegate {
    let root = XMLNode("")
    private var stack: [XMLNode] = []

    func parser(_ parser: XMLParser, didStartElement elementName: String, namespaceURI: String?,
                qualifiedName: String?, attributes: [String: String]) {
        guard stack.count < 64 else { parser.abortParsing(); return }
        let node = XMLNode(elementName, attributes: attributes)
        (stack.last ?? root).children.append(node)
        stack.append(node)
    }

    func parser(_ parser: XMLParser, foundCharacters string: String) { stack.last?.text += string }
    func parser(_ parser: XMLParser, foundCDATA data: Data) {
        stack.last?.text += String(decoding: data, as: UTF8.self)
    }
    func parser(_ parser: XMLParser, didEndElement elementName: String, namespaceURI: String?, qualifiedName: String?) {
        _ = stack.popLast()
    }
}

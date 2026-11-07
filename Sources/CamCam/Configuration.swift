import Foundation
import TOMLDecoder

struct Configuration: Decodable {
    let cameras: [Camera]

    static func load(_ url: URL) throws -> Self {
        try TOMLDecoder().decode(Self.self, from: Data(contentsOf: url))
    }
}

struct Camera: Decodable {
    enum Kind: String, Decodable {
        case rtmp, rtsp, onvif

        var schemes: [String] {
            switch self {
            case .rtmp: ["rtmp", "rtmps"]
            case .rtsp: ["rtsp"]
            case .onvif: ["http", "https"]
            }
        }
    }

    let name: String
    let type: Kind
    let url: String
    let username: String?
    let passwordEnv: String?

    enum CodingKeys: String, CodingKey {
        case name, type, url, username
        case passwordEnv = "password_env"
    }

    func resolve() async throws -> Streams {
        guard let endpoint = URL(string: url), endpoint.host != nil,
              type.schemes.contains(endpoint.scheme?.lowercased() ?? "") else {
            throw CameraError("Invalid \(type.rawValue.uppercased()) URL")
        }

        var password: String?
        if let passwordEnv {
            guard let value = ProcessInfo.processInfo.environment[passwordEnv] else {
                throw CameraError("Missing password environment variable")
            }
            password = value
        }

        let credentials = username.map { Credentials(user: $0, password: password ?? "") }
        if type == .onvif {
            return try await ONVIF(device: endpoint, credentials: credentials).streams()
        }
        return Streams(wall: credentials?.apply(to: endpoint) ?? endpoint)
    }
}

struct Streams {
    let wall: URL
    var focus: URL?
}

struct CameraError: LocalizedError {
    let errorDescription: String?
    init(_ message: String) { errorDescription = message }
}

struct Credentials {
    let user: String
    let password: String

    func apply(to url: URL) -> URL {
        guard var parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.user == nil else { return url }
        parts.user = user
        parts.password = password
        return parts.url ?? url
    }
}

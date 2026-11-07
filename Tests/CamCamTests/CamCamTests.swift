import CCamVideo
import CoreVideo
import Foundation
import Testing
@testable import CamCam

struct CamCamTests {
    @Test
    func testExampleConfiguration() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let config = try Configuration.load(root.appending(path: "config.example.toml"))
        #expect(config.cameras.map(\.type) == [.onvif, .rtmp, .rtsp])
        #expect(config.cameras.first?.passwordEnv == "CAMCAM_FRONT_DOOR_PASSWORD")
    }

    @Test
    func testCredentialEscapingAndExistingCredentials() {
        let credentials = Credentials(user: "user@home", password: "a/b?#@&")
        let url = credentials.apply(to: URL(string: "rtsp://127.0.0.1:18554/main")!)
        let parts = URLComponents(url: url, resolvingAgainstBaseURL: false)!
        #expect(parts.user == credentials.user)
        #expect(parts.password == credentials.password)
        #expect(parts.path == "/main")
        let original = URL(string: "rtsp://existing:secret@127.0.0.1/main")!
        #expect(credentials.apply(to: original) == original)
    }

    @Test
    func testInvalidSchemeAndMissingEnvironment() async throws {
        for text in [
            """
            [[cameras]]
            name = "invalid"
            type = "onvif"
            url = "rtmp://127.0.0.1/live"
            """,
            """
            [[cameras]]
            name = "invalid"
            type = "rtsp"
            url = "rtsp://127.0.0.1/live"
            password_env = "CAMCAM_TEST_UNSET_08DFD9"
            """
        ] {
            let file = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
            try text.write(to: file, atomically: true, encoding: .utf8)
            defer { try? FileManager.default.removeItem(at: file) }
            let camera = try #require(Configuration.load(file).cameras.first)
            do {
                _ = try await camera.resolve()
                Issue.record("Expected a configuration error")
            } catch is CameraError {}
        }
    }

    // Opt in with the local fixture servers described in README.md.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["CAMCAM_TEST_FEEDS"] == "1"))
    func testONVIFAndHardwareLiveStreams() async throws {
        let credentials = Credentials(user: "viewer", password: "fixture-password")
        for path in ["onvif", "legacy"] {
            let streams = try await ONVIF(
                device: URL(string: "http://127.0.0.1:18080/\(path)")!, credentials: credentials
            ).streams()
            #expect(streams.wall.path == "/sub")
            #expect(streams.focus?.path == "/main")
            #expect(streams.wall.user == credentials.user)
        }

        cam_video_initialize()
        for (url, width) in [
            ("rtmp://127.0.0.1:11935/workshop", 640),
            ("rtsp://127.0.0.1:18554/main", 1280),
            ("rtsp://127.0.0.1:18554/sub", 480),
        ] {
            // Rejoin repeatedly to exercise joining between keyframes.
            for _ in 0..<3 {
                let probe = try #require(DecodeProbe())
                var message = [CChar](repeating: 0, count: 256)
                let start = Date()
                cam_video_run(probe.session, url, { context, frame in
                    guard let context, let frame else { return }
                    let probe = Unmanaged<DecodeProbe>.fromOpaque(context).takeUnretainedValue()
                    if probe.frames == 0 {
                        probe.firstFrame = Date()
                        probe.width = CVPixelBufferGetWidth(frame)
                    }
                    probe.frames += 1
                    if probe.frames == 20 { cam_video_stop(probe.session) }
                }, Unmanaged.passUnretained(probe).toOpaque(), &message, message.count)
                #expect(probe.frames == 20, Comment(rawValue: String(cString: message)))
                #expect(probe.width == width)
                if let first = probe.firstFrame {
                    print("Hardware first frame \(url): \(String(format: "%.3f", first.timeIntervalSince(start)))s")
                }
            }
        }
    }
}

private final class DecodeProbe {
    let session: OpaquePointer
    var frames = 0
    var width = 0
    var firstFrame: Date?

    init?() {
        guard let session = cam_video_create() else { return nil }
        self.session = session
    }
    deinit { cam_video_destroy(session) }
}

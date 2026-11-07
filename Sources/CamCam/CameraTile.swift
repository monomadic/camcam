import AppKit
import AVFoundation

@MainActor
final class CameraTile: NSView {
    private let camera: Camera
    private let wallVideo = VideoSurface()
    private let focusVideo = VideoSurface()
    private let title: NSTextField
    private let status = NSTextField(labelWithString: "CONNECTING")
    private let dot = CALayer()
    private var feedTask: Task<Void, Never>?
    private var player: VideoPlayer?
    private var focusPlayer: VideoPlayer?
    private var focusURL: URL?
    private var focused = false
    private var retryDelay = 1
    var activate: (() -> Void)?

    init(_ camera: Camera) {
        self.camera = camera
        title = NSTextField(labelWithString: camera.name)
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor(white: 0.085, alpha: 1).cgColor
        layer?.cornerRadius = 8
        layer?.masksToBounds = true
        layer?.borderWidth = 1
        layer?.borderColor = NSColor(white: 0.22, alpha: 1).cgColor

        for video in [wallVideo, focusVideo] {
            video.videoGravity = .resizeAspect
            video.backgroundColor = NSColor.black.cgColor
            layer?.addSublayer(video)
        }
        focusVideo.isHidden = true
        layer?.addSublayer(dot)
        dot.cornerRadius = 3

        title.font = .systemFont(ofSize: 13, weight: .semibold)
        title.lineBreakMode = .byTruncatingTail
        status.font = .monospacedSystemFont(ofSize: 10, weight: .medium)
        status.alignment = .right
        addSubview(title)
        addSubview(status)
        setStatus("CONNECTING", color: .systemYellow)
        setAccessibilityElement(true)
        setAccessibilityRole(.button)
        setAccessibilityLabel(camera.name)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func layout() {
        super.layout()
        let footer: CGFloat = 35
        let videoFrame = CGRect(x: 0, y: footer, width: bounds.width, height: max(0, bounds.height - footer))
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        wallVideo.frame = videoFrame
        focusVideo.frame = videoFrame
        dot.frame = CGRect(x: 13, y: 15, width: 6, height: 6)
        CATransaction.commit()
        let statusWidth = min(status.intrinsicContentSize.width, bounds.width * 0.46)
        status.frame = CGRect(x: bounds.width - statusWidth - 12, y: 10, width: statusWidth, height: 16)
        title.frame = CGRect(x: 26, y: 9, width: max(0, status.frame.minX - 34), height: 18)
    }

    override func hitTest(_ point: NSPoint) -> NSView? { super.hitTest(point) == nil ? nil : self }
    override func mouseDown(with event: NSEvent) { activate?() }
    override func accessibilityPerformPress() -> Bool { activate?(); return true }

    func setFocused(_ focused: Bool) {
        self.focused = focused
        layer?.borderWidth = focused ? 2 : 1
        layer?.borderColor = (focused ? NSColor.controlAccentColor : NSColor(white: 0.22, alpha: 1)).cgColor
        if focused { startFocus() } else { stopFocus() }
    }

    func connect(after delay: Int = 0) {
        feedTask?.cancel()
        feedTask = Task { [weak self, camera] in
            do {
                if delay > 0 { try await Task.sleep(for: .seconds(delay)) }
                let streams = try await camera.resolve()
                try Task.checkCancellation()
                self?.play(streams)
            } catch is CancellationError {
                return
            } catch {
                self?.retry(error.localizedDescription)
            }
        }
    }

    func stop() {
        feedTask?.cancel()
        player?.stop()
        stopFocus()
    }

    private func play(_ streams: Streams) {
        guard let player = VideoPlayer() else { retry("Could not allocate a video session"); return }
        self.player = player
        focusURL = streams.focus
        var hasFrame = false
        player.start(streams.wall, onFrame: { [weak self] image in
            guard let self else { return }
            if !hasFrame {
                hasFrame = true
                self.retryDelay = 1
                self.setStatus("LIVE · VT", color: .systemGreen)
                if self.focused { self.startFocus() }
            }
            if !self.isHidden { self.wallVideo.display(image) }
        }, onEnd: { [weak self] reason in
            self?.retry(reason)
        })
    }

    private func retry(_ reason: String) {
        player?.stop()
        player = nil
        focusURL = nil
        stopFocus()
        setStatus("RETRYING", color: .systemOrange, detail: reason)
        let delay = retryDelay
        retryDelay = min(retryDelay * 2, 30)
        connect(after: delay)
    }

    private func startFocus() {
        guard focusPlayer == nil, let focusURL, let player = VideoPlayer() else { return }
        focusPlayer = player
        player.start(focusURL, onFrame: { [weak self] image in
            guard let self else { return }
            self.focusVideo.display(image)
            self.focusVideo.isHidden = false
        }, onEnd: { [weak self] reason in
            self?.stopFocus()
            self?.setStatus("LIVE · VT", color: .systemGreen, detail: "Main stream: \(reason)")
        })
    }

    private func stopFocus() {
        focusPlayer?.stop()
        focusPlayer = nil
        focusVideo.isHidden = true
        focusVideo.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
    }

    private func setStatus(_ label: String, color: NSColor, detail: String? = nil) {
        status.stringValue = label
        status.textColor = color
        dot.backgroundColor = color.cgColor
        toolTip = detail.map { "\(camera.name) — \($0)" } ?? camera.name
        setAccessibilityValue(detail.map { "\(label): \($0)" } ?? label)
        needsLayout = true
    }
}

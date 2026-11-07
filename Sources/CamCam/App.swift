import AppKit
import CCamVideo

@MainActor
private final class CameraWall: NSView {
    private let tiles: [CameraTile]
    private let heading = NSTextField(labelWithString: "CAMERA WALL")
    private let hint = NSTextField(labelWithString: "")
    private let empty = NSTextField(wrappingLabelWithString: "")
    private var selection: Int?

    init(cameras: [Camera], message: String?) {
        tiles = cameras.map(CameraTile.init)
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor(white: 0.045, alpha: 1).cgColor
        heading.font = .monospacedSystemFont(ofSize: 14, weight: .bold)
        hint.font = .monospacedSystemFont(ofSize: 11, weight: .medium)
        hint.textColor = .secondaryLabelColor
        hint.alignment = .right
        empty.font = .systemFont(ofSize: 15)
        empty.textColor = .secondaryLabelColor
        empty.alignment = .center
        empty.stringValue = message ?? "Add cameras to ~/.config/camcam/config.toml and relaunch."
        empty.isHidden = !tiles.isEmpty
        addSubview(heading)
        addSubview(hint)
        addSubview(empty)
        for (index, tile) in tiles.enumerated() {
            addSubview(tile)
            tile.activate = { [weak self] in self?.select(index) }
            tile.connect()
        }
        updateHeader()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func stop() { tiles.forEach { $0.stop() } }

    func exitFocus() -> Bool {
        guard let selection else { return false }
        tiles[selection].setFocused(false)
        self.selection = nil
        updateHeader()
        needsLayout = true
        return true
    }

    private func select(_ index: Int) {
        if selection == index {
            _ = exitFocus()
            return
        }
        selection = index
        tiles[index].setFocused(true)
        updateHeader()
        needsLayout = true
    }

    private func updateHeader() {
        heading.stringValue = selection == nil ? "CAMERA WALL" : "CAMERA FOCUS"
        hint.stringValue = selection == nil ? "\(tiles.count) CAMERAS" : "ESC  /  WALL"
    }

    override func layout() {
        super.layout()
        let margin: CGFloat = 18
        let gap: CGFloat = 10
        heading.frame = CGRect(x: margin, y: bounds.height - 35, width: 165, height: 19)
        hint.frame = CGRect(x: bounds.width - margin - 165, y: bounds.height - 34, width: 165, height: 17)
        let content = CGRect(x: margin, y: margin, width: bounds.width - 2 * margin,
                             height: bounds.height - 66)
        empty.frame = CGRect(x: content.minX, y: content.midY - 45, width: content.width, height: 90)
        if let selection {
            for (index, tile) in tiles.enumerated() { tile.isHidden = index != selection }
            tiles[selection].frame = content
            return
        }
        guard !tiles.isEmpty else { return }
        // Choose the layout that gives a 16:9 feed the most room after its footer.
        let columns = (1...tiles.count).max { left, right in
            func videoWidth(_ columns: Int) -> CGFloat {
                let rows = (tiles.count + columns - 1) / columns
                let width = (content.width - CGFloat(columns - 1) * gap) / CGFloat(columns)
                let height = (content.height - CGFloat(rows - 1) * gap) / CGFloat(rows)
                return min(width, (height - 35) * 16 / 9)
            }
            return videoWidth(left) < videoWidth(right)
        } ?? 1
        let rows = (tiles.count + columns - 1) / columns
        let width = max(0, (content.width - CGFloat(columns - 1) * gap) / CGFloat(columns))
        let height = max(0, (content.height - CGFloat(rows - 1) * gap) / CGFloat(rows))
        for (index, tile) in tiles.enumerated() {
            tile.isHidden = false
            let x = content.minX + CGFloat(index % columns) * (width + gap)
            let y = content.maxY - height - CGFloat(index / columns) * (height + gap)
            tile.frame = CGRect(x: x, y: y, width: width, height: height)
        }
    }
}

@MainActor
private final class CameraWindow: NSWindow {
    override func cancelOperation(_ sender: Any?) {
        if (contentView as? CameraWall)?.exitFocus() != true { super.cancelOperation(sender) }
    }
}

@MainActor
private final class AppDelegate: NSObject, NSApplicationDelegate {
    let configURL: URL
    var window: NSWindow?

    init(configURL: URL) { self.configURL = configURL }

    func applicationDidFinishLaunching(_ notification: Notification) {
        cam_video_initialize()
        let cameras: [Camera]
        let message: String?
        do {
            cameras = try Configuration.load(configURL).cameras
            message = cameras.isEmpty ? "No cameras configured.\n\(configURL.path)" : nil
        } catch {
            cameras = []
            message = "Could not read camera configuration.\n\(configURL.path)"
            // Config parser errors can include lines containing credentials.
            FileHandle.standardError.write(Data("CamCam: could not load camera configuration\n".utf8))
        }

        let menu = NSMenu()
        let appMenu = NSMenuItem()
        menu.addItem(appMenu)
        appMenu.submenu = NSMenu()
        appMenu.submenu?.addItem(withTitle: "Quit CamCam", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        NSApp.mainMenu = menu

        let window = CameraWindow(contentRect: CGRect(x: 0, y: 0, width: 1000, height: 640),
                                  styleMask: [.titled, .closable, .miniaturizable, .resizable],
                                  backing: .buffered, defer: false)
        window.title = "CamCam"
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentMinSize = NSSize(width: 460, height: 320)
        window.contentView = CameraWall(cameras: cameras, message: message)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        self.window = window
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
    func applicationWillTerminate(_ notification: Notification) { (window?.contentView as? CameraWall)?.stop() }
}

@main
enum CamCam {
    @MainActor
    static func main() {
        let args = Array(CommandLine.arguments.dropFirst())
        let configURL: URL
        switch args {
        case []:
            configURL = FileManager.default.homeDirectoryForCurrentUser.appending(path: ".config/camcam/config.toml")
        case ["--help"], ["-h"]:
            print("Usage: camcam [--config PATH]")
            return
        case let options where options.count == 2 && options[0] == "--config":
            configURL = URL(fileURLWithPath: (options[1] as NSString).expandingTildeInPath)
        default:
            FileHandle.standardError.write(Data("Usage: camcam [--config PATH]\n".utf8))
            exit(2)
        }
        let app = NSApplication.shared
        let delegate = AppDelegate(configURL: configURL)
        app.setActivationPolicy(.regular)
        app.delegate = delegate
        withExtendedLifetime(delegate) { app.run() }
    }
}

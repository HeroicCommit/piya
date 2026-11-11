import AppKit
import SwiftUI

/// Floating Hummingbird window with resize + system fullscreen (green traffic light).
/// Uses real fullscreen so the menu bar / system status icons auto-hide like other apps.
@MainActor
final class HummingbirdPanelController: NSObject, NSWindowDelegate {
    static let shared = HummingbirdPanelController()

    private let model = ChatViewModel()
    private var window: NSWindow?
    private var monitor: Any?

    private override init() {
        super.init()
        registerHotkey()
    }

    func toggle() {
        if window?.isVisible == true {
            hide()
        } else {
            show()
        }
    }

    func show() {
        if window == nil {
            buildWindow()
        }
        if window?.isVisible != true {
            positionIfNeeded()
        }
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        DispatchQueue.main.async {
            if let field = Self.findComposer(in: self.window?.contentView) {
                self.window?.makeFirstResponder(field)
            }
        }
    }

    private static func findComposer(in view: NSView?) -> NSView? {
        guard let view else { return nil }
        if view is ComposerNSTextField { return view }
        for sub in view.subviews {
            if let found = findComposer(in: sub) { return found }
        }
        return nil
    }

    func hide() {
        guard let window else { return }
        if window.styleMask.contains(.fullScreen) {
            window.toggleFullScreen(nil)
        }
        window.orderOut(nil)
    }

    private func buildWindow() {
        let win = HummingbirdWindow(
            contentRect: NSRect(x: 0, y: 0, width: 580, height: 520),
            styleMask: [
                .titled,
                .closable,
                .miniaturizable,
                .resizable,
                .fullSizeContentView,
            ],
            backing: .buffered,
            defer: false
        )
        win.delegate = self
        win.level = .floating
        // Primary fullscreen: green button enters Space fullscreen → menu bar auto-hides.
        win.collectionBehavior = [.fullScreenPrimary, .managed]
        win.titlebarAppearsTransparent = true
        win.titleVisibility = .hidden
        win.isMovableByWindowBackground = true
        win.isOpaque = false
        win.backgroundColor = .clear
        win.hasShadow = true
        win.isReleasedWhenClosed = false
        win.minSize = NSSize(width: 440, height: 420)

        win.standardWindowButton(.closeButton)?.isHidden = false
        win.standardWindowButton(.miniaturizeButton)?.isHidden = false
        if let zoom = win.standardWindowButton(.zoomButton) {
            zoom.isHidden = false
            zoom.isEnabled = true
        }

        // Visual effect lives in AppKit so SwiftUI never measures material.
        let backdrop = NSVisualEffectView()
        backdrop.material = .hudWindow
        backdrop.blendingMode = .behindWindow
        backdrop.state = .active
        backdrop.autoresizingMask = [.width, .height]

        let hosting = PinnedHostingView(rootView: HummingbirdView(model: model))
        hosting.sizingOptions = []
        hosting.translatesAutoresizingMaskIntoConstraints = false

        backdrop.addSubview(hosting)
        NSLayoutConstraint.activate([
            hosting.leadingAnchor.constraint(equalTo: backdrop.leadingAnchor),
            hosting.trailingAnchor.constraint(equalTo: backdrop.trailingAnchor),
            hosting.topAnchor.constraint(equalTo: backdrop.topAnchor),
            hosting.bottomAnchor.constraint(equalTo: backdrop.bottomAnchor),
        ])
        win.contentView = backdrop
        self.window = win
    }

    private func positionIfNeeded() {
        guard let window, let screen = NSScreen.main else { return }
        if window.frame.origin != .zero, window.isVisible { return }
        let frame = screen.visibleFrame
        let w = window.frame.width
        let h = window.frame.height
        let x = frame.midX - w / 2
        let y = frame.maxY - h - 72
        window.setFrameOrigin(NSPoint(x: x, y: y))
    }

    // MARK: - Fullscreen lifecycle

    func windowWillEnterFullScreen(_ notification: Notification) {
        // Floating level fights fullscreen Spaces; drop to normal while fullscreen.
        window?.level = .normal
    }

    func windowDidExitFullScreen(_ notification: Notification) {
        window?.level = .floating
    }

    private func registerHotkey() {
        // ⌃⌘Space — Hummingbird shortcut (local monitor; global requires accessibility permission).
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            if flags == [.control, .command], event.charactersIgnoringModifiers == " " {
                self?.toggle()
                return nil
            }
            if event.keyCode == 53 { // Esc
                self?.hide()
                return nil
            }
            return event
        }
    }
}

/// Keyable floating window so resize edges and zoom behave like a normal app window.
final class HummingbirdWindow: NSWindow {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

/// Never report an intrinsic size — AppKit pins us to the window instead.
private final class PinnedHostingView<Content: View>: NSHostingView<Content> {
    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: NSView.noIntrinsicMetric)
    }
}

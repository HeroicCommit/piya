import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        PiyaBackend.bootstrap()
        NSApp.setActivationPolicy(.accessory)
        TaskNotificationWatcher.shared.start()
        DueTaskScheduler.shared.start()
        if CommandLine.arguments.contains("--open-chat") {
            DispatchQueue.main.async {
                HummingbirdPanelController.shared.show()
            }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}

/// Opens Settings and brings it to the front.
///
/// Piya runs as an accessory (menu-bar) app, so `SettingsLink` alone opens the window
/// behind the frontmost application. Activate the app first, open the scene, then
/// raise the window once SwiftUI has created it.
private struct SettingsMenuButton: View {
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        Button("Settings…") {
            NSApp.activate(ignoringOtherApps: true)
            openSettings()
            // The window is created asynchronously; raise it right away and again
            // shortly after in case it appeared late.
            DispatchQueue.main.async { Self.raiseSettingsWindow() }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { Self.raiseSettingsWindow() }
        }
        .keyboardShortcut(",")
    }

    private static func raiseSettingsWindow() {
        NSApp.activate(ignoringOtherApps: true)
        // Settings is an ordinary titled window. Skip the Hummingbird chat window (so
        // we never pop it open) and anything hidden.
        let candidates = NSApp.windows.filter {
            $0.styleMask.contains(.titled)
                && !($0 is HummingbirdWindow)
                && !($0 is NSPanel)
                && $0.canBecomeKey
                && $0.isVisible
        }
        for window in candidates {
            window.makeKeyAndOrderFront(nil)
            window.orderFrontRegardless()
        }
    }
}

@main
struct PiyaApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        MenuBarExtra {
            Button("Ask Piya") {
                HummingbirdPanelController.shared.show()
            }
            .keyboardShortcut(" ", modifiers: [.control, .command])

            SettingsMenuButton()

            Divider()

            Button("Quit Piya") {
                NSApplication.shared.terminate(nil)
            }
        } label: {
            Image(nsImage: Self.menuBarIcon)
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView()
        }
    }

    /// Pink blossom from `AppIcon` — same mark as the original tray / app icon.
    private static let menuBarIcon: NSImage = {
        let source =
            NSImage(named: "AppIcon")
            ?? Bundle.main.image(forResource: "AppIcon")
            ?? Bundle.module.image(forResource: "AppIcon")
            ?? NSImage(systemSymbolName: "sparkles", accessibilityDescription: "Piya")!

        let size = NSSize(width: 18, height: 18)
        let icon = NSImage(size: size)
        icon.lockFocus()
        source.draw(
            in: NSRect(origin: .zero, size: size),
            from: NSRect(origin: .zero, size: source.size),
            operation: .sourceOver,
            fraction: 1.0
        )
        icon.unlockFocus()
        icon.isTemplate = false
        return icon
    }()
}

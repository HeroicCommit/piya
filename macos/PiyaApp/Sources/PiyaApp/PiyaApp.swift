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

@main
struct PiyaApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        MenuBarExtra {
            Button("Ask Piya") {
                HummingbirdPanelController.shared.show()
            }
            .keyboardShortcut(" ", modifiers: [.control, .command])

            SettingsLink {
                Text("Settings")
            }

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

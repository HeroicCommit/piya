import AppKit
import SwiftUI

/// Major alerts: a panel in the middle of the screen that floats above every app
/// (including full-screen ones) on every Space, and stays until dismissed.
@MainActor
final class MajorAlertPresenter {
    static let shared = MajorAlertPresenter()

    private var panels: [NSPanel] = []

    func show(title: String, body: String) {
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 440, height: 220),
            styleMask: [.titled, .fullSizeContentView, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isMovableByWindowBackground = true
        panel.level = .screenSaver
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.standardWindowButton(.closeButton)?.isHidden = true
        panel.standardWindowButton(.miniaturizeButton)?.isHidden = true
        panel.standardWindowButton(.zoomButton)?.isHidden = true

        let view = MajorAlertView(
            title: title,
            message: body,
            onDone: { [weak self, weak panel] in
                guard let panel else { return }
                self?.close(panel)
            },
            onSnooze: { [weak self, weak panel] minutes in
                _ = try? PiyaBackend.snoozeAlert(title: title, body: body, minutes: UInt32(minutes))
                guard let panel else { return }
                self?.close(panel)
            }
        )
        panel.contentView = NSHostingView(rootView: view)
        panel.setContentSize(panel.contentView?.fittingSize ?? NSSize(width: 440, height: 220))
        position(panel)

        panels.append(panel)
        panel.orderFrontRegardless()
        panel.makeKey()
        NSSound(named: "Glass")?.play()
        NSApp.requestUserAttention(.criticalRequest)
    }

    /// Center on the active screen; stack later alerts slightly lower.
    private func position(_ panel: NSPanel) {
        let screen = NSScreen.main ?? NSScreen.screens.first
        guard let frame = screen?.visibleFrame else { return }
        let size = panel.frame.size
        let offset = CGFloat(panels.count) * 28
        panel.setFrameOrigin(NSPoint(
            x: frame.midX - size.width / 2 + offset,
            y: frame.midY - size.height / 2 + frame.height * 0.12 - offset
        ))
    }

    private func close(_ panel: NSPanel) {
        panel.orderOut(nil)
        panels.removeAll { $0 === panel }
    }
}

private struct MajorAlertView: View {
    let title: String
    let message: String
    let onDone: () -> Void
    let onSnooze: (Int) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                Image(systemName: "bell.badge.fill")
                    .font(.system(size: 26))
                    .foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Piya")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(title)
                        .font(.title3.weight(.semibold))
                        .lineLimit(2)
                }
            }
            if !message.isEmpty && message != title {
                Text(message)
                    .font(.body)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Menu("Snooze") {
                    Button("5 minutes") { onSnooze(5) }
                    Button("10 minutes") { onSnooze(10) }
                    Button("30 minutes") { onSnooze(30) }
                    Button("1 hour") { onSnooze(60) }
                }
                .fixedSize()
                Spacer()
                Button("Done", action: onDone)
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(22)
        .frame(width: 440)
    }
}

import AppKit
import Foundation
import UserNotifications

/// Delivers Notification Center banners for Piya (task finished, chat `piya_notify`, etc.).
/// Must post from this process so banners use the Piya app icon — not osascript/Script Editor.
@MainActor
final class TaskNotificationWatcher: NSObject, UNUserNotificationCenterDelegate {
    static let shared = TaskNotificationWatcher()

    private var timer: Timer?
    private var lastPayload: String?
    private var dataDir: String?
    private var authorized = false

    private override init() {
        super.init()
    }

    func start() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self

        center.requestAuthorization(options: [.alert, .sound, .badge]) { [weak self] granted, error in
            Task { @MainActor in
                self?.authorized = granted
                if let error {
                    NSLog("Piya notification auth error: %@", error.localizedDescription)
                }
                if !granted {
                    NSLog("Piya notifications not authorized — enable in System Settings → Notifications → Piya")
                }
            }
        }

        dataDir = PiyaBackend.dataDirectory
        lastPayload = readSignal()

        timer?.invalidate()
        // Fast poll so chat `piya_notify` feels instant.
        timer = Timer.scheduledTimer(withTimeInterval: 0.4, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.poll()
            }
        }
        if let timer {
            RunLoop.main.add(timer, forMode: .common)
        }
    }

    /// Immediate banner (also used after FFI notify if desired).
    func postNow(title: String, body: String) {
        post(title: title, body: body)
    }

    private func poll() {
        guard let raw = readSignal(), raw != lastPayload else { return }
        lastPayload = raw

        guard let data = raw.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let event = json["event"] as? String
        else { return }

        // Ignore stale signals from previous runs.
        if let at = json["at"] as? String {
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            var signalDate = formatter.date(from: at)
            if signalDate == nil {
                formatter.formatOptions = [.withInternetDateTime]
                signalDate = formatter.date(from: at)
            }
            if let signalDate, Date().timeIntervalSince(signalDate) > 90 {
                return
            }
        }

        let title = (json["title"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "Piya"
        let detail = (json["detail"] as? String) ?? ""

        switch event {
        case "notify":
            post(title: title, body: detail.isEmpty ? "Notification" : detail)
        case "finished":
            post(title: title, body: detail.isEmpty ? "Task finished" : detail)
        case "started":
            // Quiet — avoid noise when tasks begin.
            break
        default:
            if !detail.isEmpty {
                post(title: title, body: detail)
            }
        }
    }

    private func readSignal() -> String? {
        guard let dataDir else { return nil }
        let path = (dataDir as NSString).appendingPathComponent("badge-signal.json")
        return try? String(contentsOfFile: path, encoding: .utf8)
    }

    private func post(title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = "Piya"
        content.subtitle = title == "Piya" ? "" : title
        content.body = body
        content.sound = .default

        let request = UNNotificationRequest(
            identifier: "piya-\(UUID().uuidString)",
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request) { error in
            if let error {
                NSLog("Piya notification deliver error: %@", error.localizedDescription)
            }
        }
    }

    // Show banner even while Hummingbird / menu bar app is focused.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound, .list]
    }
}

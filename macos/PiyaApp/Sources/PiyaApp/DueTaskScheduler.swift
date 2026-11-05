import AppKit
import Foundation

/// Polls for due scheduled tasks while the menu-bar app is running.
/// Without this, reminders only fire when Hummingbird/Tasks UI happens to refresh.
@MainActor
final class DueTaskScheduler {
    static let shared = DueTaskScheduler()

    /// Check often enough that "notify at 6:30" lands within a few seconds of 6:30.
    private static let interval: TimeInterval = 5

    private var timer: Timer?
    private var activity: NSObjectProtocol?

    private init() {}

    func start() {
        stop()

        // Keep the poller honest under App Nap while Piya stays in the menu bar.
        activity = ProcessInfo.processInfo.beginActivity(
            options: [.userInitiatedAllowingIdleSystemSleep, .latencyCritical],
            reason: "Piya due-task scheduler"
        )

        timer = Timer.scheduledTimer(withTimeInterval: Self.interval, repeats: true) { _ in
            Task { @MainActor in
                PiyaBackend.runPendingTasks()
            }
        }
        if let timer {
            RunLoop.main.add(timer, forMode: .common)
        }

        // Catch anything already due at launch.
        PiyaBackend.runPendingTasks()
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        if let activity {
            ProcessInfo.processInfo.endActivity(activity)
            self.activity = nil
        }
    }
}

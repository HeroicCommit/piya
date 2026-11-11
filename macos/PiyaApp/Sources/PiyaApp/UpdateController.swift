import AppKit
import Foundation
import PiyaBindings

enum SettingsTab: Hashable {
    case general
    case you
    case memory
    case notifications
    case shell
    case mcp
}

@MainActor
final class UpdateController: ObservableObject {
    static let shared = UpdateController()

    enum Phase: Equatable {
        case idle
        case checking
        case upToDate(version: String)
        case available(
            current: String,
            latest: String,
            notes: String,
            url: String,
            hasAsset: Bool
        )
        case installing(version: String)
        case failed(String)
    }

    @Published private(set) var phase: Phase = .idle
    @Published var selectedSettingsTab: SettingsTab = .general

    private var periodicTimer: Timer?

    private init() {}

    func startBackgroundChecks() {
        periodicTimer?.invalidate()
        // Check quietly shortly after launch, then every 6 hours.
        DispatchQueue.main.asyncAfter(deadline: .now() + 12) { [weak self] in
            self?.check(silent: true)
        }
        let timer = Timer.scheduledTimer(withTimeInterval: 6 * 3600, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.check(silent: true)
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        periodicTimer = timer
    }

    var availableVersion: String? {
        if case .available(_, let latest, _, _, _) = phase {
            return latest
        }
        return nil
    }

    func check(silent: Bool = false) {
        if case .checking = phase { return }
        if case .installing = phase { return }

        if !silent {
            phase = .checking
        }

        Task.detached(priority: .utility) {
            let result = Result { try PiyaBackend.checkForUpdate() }
            await MainActor.run {
                let controller = UpdateController.shared
                switch result {
                case .success(let info):
                    if info.updateAvailable {
                        controller.phase = .available(
                            current: info.currentVersion,
                            latest: info.latestVersion,
                            notes: info.releaseNotes,
                            url: info.releaseUrl,
                            hasAsset: info.hasAsset
                        )
                    } else if !silent {
                        controller.phase = .upToDate(version: info.currentVersion)
                    }
                case .failure(let error):
                    if !silent {
                        controller.phase = .failed(error.localizedDescription)
                    }
                }
            }
        }
    }

    func installAndRelaunch() {
        let targetVersion = availableVersion ?? ""
        phase = .installing(version: targetVersion)

        Task.detached(priority: .userInitiated) {
            let result = Result { try PiyaBackend.installUpdateAndRelaunch() }
            await MainActor.run {
                switch result {
                case .success:
                    // Detached helper waits for our PID to exit, atomically swaps
                    // /Applications/Piya.app, and reopens Piya.
                    NSApplication.shared.terminate(nil)
                case .failure(let error):
                    UpdateController.shared.phase = .failed(error.localizedDescription)
                }
            }
        }
    }
}

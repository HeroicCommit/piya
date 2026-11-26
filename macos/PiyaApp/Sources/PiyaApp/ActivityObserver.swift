import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
import PiyaBindings
import ScreenCaptureKit
import Vision

/// Passively observes active macOS apps, window titles, URLs/document paths, and
/// (depending on user setting) Accessibility window text or on-device Vision OCR.
///
/// All data stays local in `data/piya.db` via `PiyaBackend.recordActivity(...)`.
@MainActor
final class ActivityObserver: ObservableObject {
    static let shared = ActivityObserver()

    @Published private(set) var settings = FfiActivitySettings(
        mode: "text_only",
        pausedUntil: 0,
        isPaused: false,
        excludedApps: [],
        retentionDays: 30
    )
    @Published private(set) var lastObservedLabel: String = ""
    @Published private(set) var hasAccessibilityPermission: Bool = false
    @Published private(set) var hasScreenCapturePermission: Bool = false

    private var timer: Timer?
    private var workspaceTokens: [NSObjectProtocol] = []
    private var pendingSwitchWork: DispatchWorkItem?
    private var isScreenAsleep = false
    private var lastOcrAt: Date = .distantPast
    private var lastOcrWindowKey: String = ""

    private static let heartbeatSeconds: TimeInterval = 15
    private static let maxIdleSeconds: Double = 120
    private nonisolated static let minOcrInterval: TimeInterval = 25

    private init() {
        refreshPermissions()
        reloadSettings()
    }

    func start() {
        reloadSettings()
        refreshPermissions()
        stopObservers()

        let wsCenter = NSWorkspace.shared.notificationCenter
        workspaceTokens.append(
            wsCenter.addObserver(
                forName: NSWorkspace.didActivateApplicationNotification,
                object: nil,
                queue: .main
            ) { [weak self] note in
                let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
                Task { @MainActor in
                    self?.scheduleAppSwitchSample(app: app)
                }
            }
        )
        workspaceTokens.append(
            wsCenter.addObserver(
                forName: NSWorkspace.screensDidSleepNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor in self?.isScreenAsleep = true }
            }
        )
        workspaceTokens.append(
            wsCenter.addObserver(
                forName: NSWorkspace.screensDidWakeNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor in self?.isScreenAsleep = false }
            }
        )
        workspaceTokens.append(
            wsCenter.addObserver(
                forName: NSWorkspace.sessionDidResignActiveNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor in self?.isScreenAsleep = true }
            }
        )
        workspaceTokens.append(
            wsCenter.addObserver(
                forName: NSWorkspace.sessionDidBecomeActiveNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor in self?.isScreenAsleep = false }
            }
        )

        let t = Timer.scheduledTimer(withTimeInterval: Self.heartbeatSeconds, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.heartbeat()
            }
        }
        t.tolerance = 3.0
        RunLoop.main.add(t, forMode: .common)
        timer = t

        // Take an initial snapshot shortly after launch.
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
            self?.sampleCurrentFrontmost(forceOcrIfFull: false)
        }
    }

    func stopObservers() {
        timer?.invalidate()
        timer = nil
        pendingSwitchWork?.cancel()
        pendingSwitchWork = nil
        let wsCenter = NSWorkspace.shared.notificationCenter
        for tok in workspaceTokens {
            wsCenter.removeObserver(tok)
        }
        workspaceTokens.removeAll()
    }

    func reloadSettings() {
        if let loaded = try? PiyaBackend.activitySettings() {
            settings = loaded
        }
    }

    func refreshPermissions() {
        hasAccessibilityPermission = AXIsProcessTrusted()
        hasScreenCapturePermission = CGPreflightScreenCaptureAccess()
    }

    func requestAccessibilityPermission() {
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        let opts = [key: true] as CFDictionary
        _ = AXIsProcessTrustedWithOptions(opts)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
            self?.refreshPermissions()
        }
    }

    func requestScreenCapturePermission() {
        _ = CGRequestScreenCaptureAccess()
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
            NSWorkspace.shared.open(url)
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
            self?.refreshPermissions()
        }
    }

    func pause(minutes: UInt32) {
        if let updated = try? PiyaBackend.pauseActivity(minutes: minutes) {
            settings = updated
        }
    }

    func saveSettings(mode: String, retentionDays: UInt32, excludedApps: [String]) throws {
        let updated = try PiyaBackend.saveActivitySettings(
            mode: mode,
            retentionDays: retentionDays,
            excludedApps: excludedApps
        )
        settings = updated
        refreshPermissions()
    }

    /// Called right BEFORE Hummingbird activates Piya so the window the user was just
    /// looking at is captured immediately for "summarize this" / "what am I working on?".
    func captureBeforePanelOpen() {
        guard let front = NSWorkspace.shared.frontmostApplication else { return }
        if let bid = front.bundleIdentifier, bid.hasPrefix("com.piya.swiftui") {
            return
        }
        sample(app: front, forceOcrIfFull: true, ignoreIdle: true)
    }

    // MARK: - Sampling loop

    private func scheduleAppSwitchSample(app: NSRunningApplication?) {
        pendingSwitchWork?.cancel()
        let target = app ?? NSWorkspace.shared.frontmostApplication
        guard let target else { return }
        if let bid = target.bundleIdentifier, bid.hasPrefix("com.piya.swiftui") {
            return
        }
        let work = DispatchWorkItem { [weak self] in
            Task { @MainActor in
                self?.sample(app: target, forceOcrIfFull: false, ignoreIdle: true)
            }
        }
        pendingSwitchWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4, execute: work)
    }

    private func heartbeat() {
        reloadSettings()
        refreshPermissions()
        guard !isScreenAsleep else { return }
        guard !settings.isPaused, settings.mode != "off" else { return }
        guard Self.secondsSinceUserInput() <= Self.maxIdleSeconds else { return }
        sampleCurrentFrontmost(forceOcrIfFull: false)
    }

    private func sampleCurrentFrontmost(forceOcrIfFull: Bool) {
        guard let app = NSWorkspace.shared.frontmostApplication else { return }
        if let bid = app.bundleIdentifier, bid.hasPrefix("com.piya.swiftui") {
            return
        }
        sample(app: app, forceOcrIfFull: forceOcrIfFull, ignoreIdle: false)
    }

    private func sample(app: NSRunningApplication, forceOcrIfFull: Bool, ignoreIdle: Bool) {
        if !ignoreIdle && Self.secondsSinceUserInput() > Self.maxIdleSeconds {
            return
        }
        let mode = settings.mode
        guard !settings.isPaused, mode != "off" else { return }

        let appName = app.localizedName ?? "Unknown"
        let bundleId = app.bundleIdentifier ?? ""
        let pid = app.processIdentifier
        let allowOcr = mode == "full" && hasScreenCapturePermission
        let prevOcrAt = lastOcrAt
        let prevOcrKey = lastOcrWindowKey

        Task.detached(priority: .utility) {
            let snap = await Self.captureWindowSnapshot(
                pid: pid,
                appName: appName,
                bundleId: bundleId,
                mode: mode,
                allowOcr: allowOcr,
                forceOcr: forceOcrIfFull,
                lastOcrAt: prevOcrAt,
                lastOcrKey: prevOcrKey
            )
            let recorded = (try? PiyaBackend.recordActivity(
                appName: appName,
                bundleId: bundleId,
                windowTitle: snap.windowTitle,
                urlOrPath: snap.urlOrPath,
                screenText: snap.screenText,
                captureKind: snap.captureKind
            )) ?? false

            await MainActor.run {
                if snap.didRunOcr {
                    self.lastOcrAt = Date()
                    self.lastOcrWindowKey = snap.windowKey
                }
                if recorded {
                    self.lastObservedLabel = snap.windowTitle.isEmpty
                        ? appName
                        : "\(appName) — \(snap.windowTitle)"
                }
            }
        }
    }

    // MARK: - Idle + Window / AX / Vision extraction

    private nonisolated static func secondsSinceUserInput() -> Double {
        let types: [CGEventType] = [
            .mouseMoved,
            .leftMouseDown,
            .rightMouseDown,
            .keyDown,
            .scrollWheel,
            .flagsChanged,
        ]
        var minSec = Double.greatestFiniteMagnitude
        for t in types {
            let s = CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: t)
            if s >= 0 && s < minSec {
                minSec = s
            }
        }
        return minSec == Double.greatestFiniteMagnitude ? 0 : minSec
    }

    private struct CapturedSnapshot: Sendable {
        var windowTitle: String
        var urlOrPath: String
        var screenText: String
        var captureKind: String
        var windowKey: String
        var didRunOcr: Bool
    }

    private nonisolated static func captureWindowSnapshot(
        pid: pid_t,
        appName: String,
        bundleId: String,
        mode: String,
        allowOcr: Bool,
        forceOcr: Bool,
        lastOcrAt: Date,
        lastOcrKey: String
    ) async -> CapturedSnapshot {
        // 1. CGWindowList metadata (window ID + fallback title)
        let (cgWindowId, cgTitle) = frontWindowFromCG(pid: pid)

        // 2. Accessibility metadata + optional AX text walk
        var axTitle = ""
        var urlOrPath = ""
        var axText = ""
        if AXIsProcessTrusted() {
            let ax = readAccessibilitySnapshot(pid: pid, collectBodyText: mode != "app_only")
            axTitle = ax.title
            urlOrPath = ax.urlOrPath
            axText = ax.bodyText
        }

        let finalTitle = !axTitle.isEmpty ? axTitle : cgTitle
        let windowKey = "\(bundleId)|\(cgWindowId ?? 0)|\(finalTitle)"

        if mode == "app_only" {
            return CapturedSnapshot(
                windowTitle: finalTitle,
                urlOrPath: urlOrPath,
                screenText: "",
                captureKind: "app_only",
                windowKey: windowKey,
                didRunOcr: false
            )
        }

        // 3. In `full` mode, run on-device Vision OCR if AX text is sparse or window changed
        var screenText = axText
        var captureKind = "text_only"
        var didRunOcr = false

        if allowOcr, let winId = cgWindowId {
            let elapsed = Date().timeIntervalSince(lastOcrAt)
            let windowChanged = windowKey != lastOcrKey
            let sparseAx = axText.count < 100
            if forceOcr || (sparseAx && (windowChanged || elapsed >= minOcrInterval)) || (windowChanged && elapsed >= minOcrInterval) {
                if let ocrText = await recognizeWindowTextViaVision(windowID: winId), !ocrText.isEmpty {
                    didRunOcr = true
                    captureKind = "full"
                    if axText.isEmpty {
                        screenText = ocrText
                    } else if !axText.contains(ocrText.prefix(60)) {
                        screenText = axText + "\n\n[Screen OCR]:\n" + ocrText
                    }
                }
            }
        }

        return CapturedSnapshot(
            windowTitle: finalTitle,
            urlOrPath: urlOrPath,
            screenText: screenText,
            captureKind: captureKind,
            windowKey: windowKey,
            didRunOcr: didRunOcr
        )
    }

    private nonisolated static func frontWindowFromCG(pid: pid_t) -> (CGWindowID?, String) {
        let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
        guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
            return (nil, "")
        }
        for info in list {
            guard let ownerPid = info[kCGWindowOwnerPID as String] as? pid_t, ownerPid == pid else {
                continue
            }
            let layer = (info[kCGWindowLayer as String] as? Int) ?? 0
            if layer != 0 { continue }
            let alpha = (info[kCGWindowAlpha as String] as? Double) ?? 1.0
            if alpha <= 0.05 { continue }
            if let bounds = info[kCGWindowBounds as String] as? [String: Any] {
                let w = (bounds["Width"] as? Double) ?? 0
                let h = (bounds["Height"] as? Double) ?? 0
                if w < 120 || h < 80 { continue }
            }
            let winId = (info[kCGWindowNumber as String] as? UInt32).map { CGWindowID($0) }
            let title = (info[kCGWindowName as String] as? String)?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            return (winId, title)
        }
        return (nil, "")
    }

    // MARK: - Accessibility reader

    private struct AxSnapshot {
        var title: String = ""
        var urlOrPath: String = ""
        var bodyText: String = ""
    }

    private nonisolated static func readAccessibilitySnapshot(pid: pid_t, collectBodyText: Bool) -> AxSnapshot {
        var out = AxSnapshot()
        let appElem = AXUIElementCreateApplication(pid)
        // Keep IPC timeout short (250ms) so an unresponsive app never stalls our worker queue.
        AXUIElementSetMessagingTimeout(appElem, 0.25)

        guard let winRef = copyAxAttr(appElem, kAXFocusedWindowAttribute as String)
                ?? copyAxAttr(appElem, kAXMainWindowAttribute as String)
        else {
            return out
        }
        let win = winRef as! AXUIElement
        AXUIElementSetMessagingTimeout(win, 0.25)

        if let t = copyAxString(win, kAXTitleAttribute as String) {
            out.title = t
        }
        if let doc = copyAxString(win, kAXDocumentAttribute as String), !doc.isEmpty {
            out.urlOrPath = cleanDocumentURL(doc)
        }

        // Also check focused UI element for selected text or web URL!
        var chunks: [String] = []
        var seen = Set<String>()
        func pushChunk(_ s: String, prefix: String? = nil) {
            let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.count >= 2 else { return }
            let clipped = String(trimmed.prefix(1_500))
            guard seen.insert(clipped).inserted else { return }
            if let prefix {
                chunks.append("\(prefix)\(clipped)")
            } else {
                chunks.append(clipped)
            }
        }

        if let focusedRef = copyAxAttr(appElem, kAXFocusedUIElementAttribute as String) {
            let focused = focusedRef as! AXUIElement
            if out.urlOrPath.isEmpty, let u = copyAxString(focused, "AXURL") ?? copyAxString(focused, kAXDocumentAttribute as String) {
                out.urlOrPath = cleanDocumentURL(u)
            }
            if collectBodyText {
                if let sel = copyAxString(focused, kAXSelectedTextAttribute as String), !sel.isEmpty {
                    pushChunk(sel, prefix: "[Selected]: ")
                }
                if let val = copyAxString(focused, kAXValueAttribute as String), !val.isEmpty {
                    pushChunk(val)
                }
            }
        }

        guard collectBodyText else { return out }

        // Bounded breadth-first walk of visible AX elements (<= 220 nodes, <= 4,800 chars).
        var queue: [(AXUIElement, Int)] = [(win, 0)]
        var visited = 0
        var charTotal = chunks.reduce(0) { $0 + $1.count }

        while !queue.isEmpty && visited < 220 && charTotal < 4_800 {
            let (elem, depth) = queue.removeFirst()
            visited += 1

            let role = copyAxString(elem, kAXRoleAttribute as String) ?? ""
            if role == "AXSecureTextField" {
                continue // NEVER read password fields!
            }
            if out.urlOrPath.isEmpty && role == "AXWebArea" {
                if let u = copyAxString(elem, "AXURL") {
                    out.urlOrPath = cleanDocumentURL(u)
                }
            }

            for attr in [kAXValueAttribute as String, kAXTitleAttribute as String, kAXDescriptionAttribute as String] {
                if let s = copyAxString(elem, attr) {
                    let clean = s.trimmingCharacters(in: .whitespacesAndNewlines)
                    if clean.count >= 3 && clean != out.title {
                        let before = chunks.count
                        pushChunk(clean)
                        if chunks.count > before {
                            charTotal += Swift.min(clean.count, 1_500)
                        }
                    }
                }
            }

            guard depth < 9 else { continue }
            let children = copyAxChildren(elem, kAXVisibleChildrenAttribute as String)
                ?? copyAxChildren(elem, kAXChildrenAttribute as String)
                ?? []
            for child in children.prefix(35) {
                queue.append((child, depth + 1))
            }
        }

        out.bodyText = chunks.joined(separator: "\n")
        return out
    }

    private nonisolated static func copyAxAttr(_ elem: AXUIElement, _ attr: String) -> CFTypeRef? {
        var value: CFTypeRef?
        let err = AXUIElementCopyAttributeValue(elem, attr as CFString, &value)
        return err == .success ? value : nil
    }

    private nonisolated static func copyAxString(_ elem: AXUIElement, _ attr: String) -> String? {
        guard let raw = copyAxAttr(elem, attr) else { return nil }
        if let s = raw as? String {
            return s
        }
        if let url = raw as? URL {
            return url.absoluteString
        }
        return nil
    }

    private nonisolated static func copyAxChildren(_ elem: AXUIElement, _ attr: String) -> [AXUIElement]? {
        guard let raw = copyAxAttr(elem, attr) else { return nil }
        return raw as? [AXUIElement]
    }

    private nonisolated static func cleanDocumentURL(_ raw: String) -> String {
        if raw.hasPrefix("file://"), let url = URL(string: raw) {
            return url.path
        }
        return raw
    }

    // MARK: - ScreenCaptureKit + Apple Vision on-device OCR (image discarded immediately)

    private nonisolated static func recognizeWindowTextViaVision(windowID: CGWindowID) async -> String? {
        do {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            guard let scWindow = content.windows.first(where: { $0.windowID == windowID }) else {
                return nil
            }
            let filter = SCContentFilter(desktopIndependentWindow: scWindow)
            let config = SCStreamConfiguration()
            let w = Int(scWindow.frame.width.clamped(to: 320...1920))
            let h = Int(scWindow.frame.height.clamped(to: 200...1400))
            config.width = w
            config.height = h
            config.showsCursor = false

            let cgImage = try await SCScreenshotManager.captureImage(
                contentFilter: filter,
                configuration: config
            )

            let request = VNRecognizeTextRequest()
            request.recognitionLevel = .fast
            request.usesLanguageCorrection = true

            let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
            try handler.perform([request])

            let lines = (request.results ?? [])
                .compactMap { $0.topCandidates(1).first?.string.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { $0.count >= 2 }
            if lines.isEmpty { return nil }
            return String(lines.joined(separator: "\n").prefix(5_000))
        } catch {
            return nil
        }
    }
}

private extension Comparable {
    func clamped(to limits: ClosedRange<Self>) -> Self {
        min(max(self, limits.lowerBound), limits.upperBound)
    }
}

import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
import PiyaBindings
import ScreenCaptureKit
import Vision

/// Actively observes visible windows across ALL connected macOS displays (`CGGetActiveDisplayList`),
/// capturing window/tab titles, URLs/document paths, Accessibility text (with Electron/Chromium wakeup),
/// and on-device Apple Vision OCR — synthesizing Little Bird-style work threads locally in `data/piya.db`.
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
    @Published private(set) var lastCapturedChars: Int = 0
    @Published private(set) var lastCaptureKind: String = ""
    @Published private(set) var hasAccessibilityPermission: Bool = false
    @Published private(set) var hasScreenCapturePermission: Bool = false

    var needsPermissionAttention: Bool {
        guard !settings.isPaused, settings.mode != "off" else { return false }
        if settings.mode == "full" {
            return !hasScreenCapturePermission
        }
        if settings.mode == "text_only" {
            return !hasAccessibilityPermission
        }
        return false
    }

    private var timer: Timer?
    private var journalTimer: Timer?
    private var processActivity: NSObjectProtocol?
    private var workspaceTokens: [NSObjectProtocol] = []
    private var appTokens: [NSObjectProtocol] = []
    private var pendingSwitchWork: DispatchWorkItem?
    private var isScreenAsleep = false
    /// Per-window OCR timestamps so Screen 1 and Screen 2 never throttle each other!
    private var lastOcrByWindowKey: [String: Date] = [:]

    /// Poll every 8s so browser tab switches, scrolling on secondary monitors & IDE file changes are caught quickly.
    private static let heartbeatSeconds: TimeInterval = 8
    /// Periodically digest new window + OCR observations into Little Bird-style work threads.
    private static let journalSyncSeconds: TimeInterval = 180
    private static let maxIdleSeconds: Double = 120
    /// Minimum cooldown for immediate OCR on app/window/tab switches.
    private nonisolated static let switchOcrCooldown: TimeInterval = 2.5
    /// Continuous OCR cadence while actively typing, scrolling, or reading across displays.
    private nonisolated static let activeOcrInterval: TimeInterval = 18
    /// Active user input window within which continuous OCR keeps refreshing across screens.
    private nonisolated static let activeInputWindowSeconds: Double = 40

    private nonisolated static let awakenedAxPidsLock = NSLock()
    private nonisolated(unsafe) static var awakenedAxPids = Set<pid_t>()

    private init() {
        reloadSettings()
        refreshPermissions()
    }

    func start() {
        reloadSettings()
        stopObservers()

        // Prevent App Nap from starving background OCR & multi-screen sampling while Piya lives in the menu bar.
        processActivity = ProcessInfo.processInfo.beginActivity(
            options: [.userInitiatedAllowingIdleSystemSleep],
            reason: "Piya active multi-display & OCR context observer"
        )

        refreshPermissions()

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

        let appCenter = NotificationCenter.default
        appTokens.append(
            appCenter.addObserver(
                forName: NSApplication.didBecomeActiveNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor in
                    self?.reloadSettings()
                    self?.refreshPermissions()
                }
            }
        )

        let t = Timer.scheduledTimer(withTimeInterval: Self.heartbeatSeconds, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.heartbeat()
            }
        }
        t.tolerance = 1.5
        RunLoop.main.add(t, forMode: .common)
        timer = t

        let jt = Timer.scheduledTimer(withTimeInterval: Self.journalSyncSeconds, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.triggerBackgroundJournalSync(force: false)
            }
        }
        jt.tolerance = 15.0
        RunLoop.main.add(jt, forMode: .common)
        journalTimer = jt

        // Take an initial multi-screen snapshot shortly after launch.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
            self?.sampleAllScreens(forceOcrIfFull: true, ignoreIdle: true)
        }
    }

    func stopObservers() {
        timer?.invalidate()
        timer = nil
        journalTimer?.invalidate()
        journalTimer = nil
        pendingSwitchWork?.cancel()
        pendingSwitchWork = nil
        if let processActivity {
            ProcessInfo.processInfo.endActivity(processActivity)
            self.processActivity = nil
        }
        let wsCenter = NSWorkspace.shared.notificationCenter
        for tok in workspaceTokens {
            wsCenter.removeObserver(tok)
        }
        workspaceTokens.removeAll()
        let appCenter = NotificationCenter.default
        for tok in appTokens {
            appCenter.removeObserver(tok)
        }
        appTokens.removeAll()
    }

    func reloadSettings() {
        if let loaded = try? PiyaBackend.activitySettings() {
            settings = loaded
        }
    }

    /// Strictly silent preflight checks ONLY — NEVER call SCShareableContent or CGRequest here,
    /// so macOS never pops repeated Screen & System Audio Recording dialogs.
    func refreshPermissions() {
        hasAccessibilityPermission = AXIsProcessTrusted()
        hasScreenCapturePermission = CGPreflightScreenCaptureAccess()
    }

    func relaunchApp() {
        let bundleURL = Bundle.main.bundleURL
        let config = NSWorkspace.OpenConfiguration()
        config.createsNewApplicationInstance = true
        NSWorkspace.shared.openApplication(at: bundleURL, configuration: config) { _, _ in
            DispatchQueue.main.async {
                NSApplication.shared.terminate(nil)
            }
        }
    }

    private nonisolated static func resetStaleTccEntry(service: String) {
        guard let bundleId = Bundle.main.bundleIdentifier, !bundleId.isEmpty else { return }
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/tccutil")
        proc.arguments = ["reset", service, bundleId]
        proc.standardOutput = Pipe()
        proc.standardError = Pipe()
        try? proc.run()
        proc.waitUntilExit()
    }

    func requestAccessibilityPermission(resetStaleFirst: Bool = true) {
        if resetStaleFirst && !AXIsProcessTrusted() {
            Self.resetStaleTccEntry(service: "Accessibility")
        }
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        let opts = [key: true] as CFDictionary
        _ = AXIsProcessTrustedWithOptions(opts)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
            self?.refreshPermissions()
        }
    }

    func requestScreenCapturePermission(resetStaleFirst: Bool = true) {
        if resetStaleFirst && !CGPreflightScreenCaptureAccess() {
            Self.resetStaleTccEntry(service: "ScreenCapture")
        }
        _ = CGRequestScreenCaptureAccess()
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

    func triggerBackgroundJournalSync(force: Bool) {
        guard !settings.isPaused, settings.mode != "off" else { return }
        Task.detached(priority: .utility) {
            _ = try? PiyaBackend.syncActivityJournal(force: force)
        }
    }

    /// Called right BEFORE Hummingbird activates Piya so visible windows on ALL monitors
    /// are captured immediately for "summarize this" / "what am I working on?".
    func captureBeforePanelOpen() {
        sampleAllScreens(forceOcrIfFull: true, ignoreIdle: true)
    }

    // MARK: - Multi-Display Sampling Loop

    private func scheduleAppSwitchSample(app: NSRunningApplication?) {
        pendingSwitchWork?.cancel()
        if let bid = app?.bundleIdentifier, bid.hasPrefix("com.piya.swiftui") {
            return
        }
        let work = DispatchWorkItem { [weak self] in
            Task { @MainActor in
                self?.sampleAllScreens(forceOcrIfFull: true, ignoreIdle: true, explicitFrontApp: app)
            }
        }
        pendingSwitchWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.55, execute: work)
    }

    private func heartbeat() {
        reloadSettings()
        refreshPermissions()
        guard !isScreenAsleep else { return }
        guard !settings.isPaused, settings.mode != "off" else { return }
        guard Self.secondsSinceUserInput() <= Self.maxIdleSeconds else { return }
        sampleAllScreens(forceOcrIfFull: false, ignoreIdle: false)
    }

    private struct ScreenWindowTarget: Sendable {
        var pid: pid_t
        var appName: String
        var bundleId: String
        var windowId: CGWindowID?
        var cgTitle: String
        var bounds: CGRect?
        var displayIndex: Int
        var totalDisplays: Int
        var isFocused: Bool
    }

    private func sampleAllScreens(
        forceOcrIfFull: Bool,
        ignoreIdle: Bool,
        explicitFrontApp: NSRunningApplication? = nil
    ) {
        reloadSettings()
        refreshPermissions()

        let idleSec = Self.secondsSinceUserInput()
        if !ignoreIdle && idleSec > Self.maxIdleSeconds {
            return
        }
        let mode = settings.mode
        guard !settings.isPaused, mode != "off" else { return }

        let frontApp = explicitFrontApp ?? NSWorkspace.shared.frontmostApplication
        // If Piya itself is frontmost right now, still allow sampling visible work windows on other displays
        // ONLY when explicitly requested, otherwise skip Piya focus switches.
        if !ignoreIdle, let bid = frontApp?.bundleIdentifier, bid.hasPrefix("com.piya.swiftui") {
            return
        }

        let allowOcr = mode == "full" && hasScreenCapturePermission
        let ocrCacheSnapshot = lastOcrByWindowKey
        let runningAppsByPid: [pid_t: (String, String)] = Dictionary(
            uniqueKeysWithValues: NSWorkspace.shared.runningApplications.map {
                ($0.processIdentifier, ($0.localizedName ?? "", $0.bundleIdentifier ?? ""))
            }
        )
        let frontPid = frontApp?.processIdentifier ?? 0
        let frontName = frontApp?.localizedName ?? "Unknown"
        let frontBundle = frontApp?.bundleIdentifier ?? ""

        Task.detached(priority: .utility) {
            let targets = Self.discoverTargetsAcrossAllDisplays(
                frontPid: frontPid,
                frontName: frontName,
                frontBundle: frontBundle,
                runningAppsByPid: runningAppsByPid
            )
            guard !targets.isEmpty else { return }

            // Share one SCShareableContent lookup across all monitors in this sample tick!
            let sharedSCContent: SCShareableContent? = allowOcr
                ? (try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true))
                : nil

            var updatedOcrTimes: [String: Date] = [:]
            var primaryLabel: String?
            var totalChars = 0
            var primaryKind = ""

            // Record secondary displays first, and the focused window LAST so `ORDER BY id DESC`
            // keeps the user's focused window at the very top while secondary screens stay live right below it!
            let orderedTargets = targets.sorted { (!$0.isFocused && $1.isFocused) }

            for target in orderedTargets {
                let snap = await Self.captureWindowSnapshot(
                    target: target,
                    mode: mode,
                    allowOcr: allowOcr,
                    forceOcr: forceOcrIfFull && target.isFocused,
                    idleSeconds: idleSec,
                    ocrCache: ocrCacheSnapshot,
                    sharedSCContent: sharedSCContent
                )
                let recorded = (try? PiyaBackend.recordActivity(
                    appName: target.appName,
                    bundleId: target.bundleId,
                    windowTitle: snap.windowTitle,
                    urlOrPath: snap.urlOrPath,
                    screenText: snap.screenText,
                    captureKind: snap.captureKind,
                    isFocused: target.isFocused
                )) ?? false

                if snap.didRunOcr {
                    updatedOcrTimes[snap.windowKey] = Date()
                }
                if recorded {
                    totalChars += snap.screenText.count
                    if target.isFocused || primaryLabel == nil {
                        primaryKind = snap.captureKind
                        let base = snap.windowTitle.isEmpty
                            ? target.appName
                            : "\(target.appName) — \(snap.windowTitle)"
                        primaryLabel = targets.count > 1
                            ? "\(base) (+\(targets.count - 1) screen)"
                            : base
                    }
                }
            }

            let finalOcrTimes = updatedOcrTimes
            let finalLabel = primaryLabel
            let finalChars = totalChars
            let finalKind = primaryKind

            await MainActor.run {
                for (k, v) in finalOcrTimes {
                    if self.lastOcrByWindowKey.count > 64 {
                        self.lastOcrByWindowKey.removeAll()
                    }
                    self.lastOcrByWindowKey[k] = v
                }
                if !finalOcrTimes.isEmpty {
                    self.hasScreenCapturePermission = true
                }
                if let finalLabel {
                    self.lastObservedLabel = finalLabel
                    self.lastCapturedChars = finalChars
                    self.lastCaptureKind = finalKind
                }
            }
        }
    }

    // MARK: - Multi-Display Window Discovery (Quartz Global Coordinates)

    private nonisolated static let systemIgnoredOwners: Set<String> = [
        "Window Server",
        "Dock",
        "Control Center",
        "Notification Center",
        "Spotlight",
        "SystemUIServer",
        "Wallpaper",
        "universalAccessAuthWarn",
        "CoreServicesUIAgent",
    ]

    /// Finds BOTH the focused window (whichever monitor/Space it is on) AND the topmost
    /// unobstructed application window on every other connected display (`CGGetActiveDisplayList`).
    private nonisolated static func discoverTargetsAcrossAllDisplays(
        frontPid: pid_t,
        frontName: String,
        frontBundle: String,
        runningAppsByPid: [pid_t: (String, String)]
    ) -> [ScreenWindowTarget] {
        // 1. Enumerate all active hardware/virtual displays in Quartz global top-left coordinates.
        var displayCount: UInt32 = 0
        CGGetActiveDisplayList(0, nil, &displayCount)
        var displayIDs = [CGDirectDisplayID](repeating: 0, count: Int(max(displayCount, 1)))
        CGGetActiveDisplayList(displayCount, &displayIDs, &displayCount)
        let displays: [CGRect] = displayIDs.prefix(Int(displayCount)).map { CGDisplayBounds($0) }
        let totalDisplays = max(displays.count, 1)

        // 2. Read front-to-back on-screen windows across ALL displays.
        let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
        let cgList = (CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]]) ?? []

        struct ParsedCGWin {
            var winId: CGWindowID
            var pid: pid_t
            var ownerName: String
            var title: String
            var bounds: CGRect
        }

        var onScreenWindows: [ParsedCGWin] = []
        for info in cgList {
            let layer = (info[kCGWindowLayer as String] as? NSNumber)?.intValue ?? -1
            if layer != 0 { continue }
            let alpha = (info[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 1.0
            if alpha <= 0.05 { continue }
            guard let pid = (info[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value,
                  let widNum = info[kCGWindowNumber as String] as? NSNumber,
                  let bDict = info[kCGWindowBounds as String] as? [String: Any]
            else { continue }

            let x = (bDict["X"] as? NSNumber)?.doubleValue ?? 0
            let y = (bDict["Y"] as? NSNumber)?.doubleValue ?? 0
            let w = (bDict["Width"] as? NSNumber)?.doubleValue ?? 0
            let h = (bDict["Height"] as? NSNumber)?.doubleValue ?? 0
            if w < 160 || h < 100 { continue }

            let owner = (info[kCGWindowOwnerName as String] as? String) ?? ""
            if systemIgnoredOwners.contains(owner) { continue }
            let bundle = runningAppsByPid[pid]?.1 ?? ""
            if bundle.hasPrefix("com.piya.swiftui") || owner.lowercased() == "piya" {
                continue
            }

            let title = (info[kCGWindowName as String] as? String)?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            onScreenWindows.append(
                ParsedCGWin(
                    winId: CGWindowID(widNum.uint32Value),
                    pid: pid,
                    ownerName: owner,
                    title: title,
                    bounds: CGRect(x: x, y: y, width: w, height: h)
                )
            )
        }

        func displayIndexForRect(_ rect: CGRect) -> Int {
            var bestIdx = 0
            var bestArea: CGFloat = -1
            for (idx, dRect) in displays.enumerated() {
                let inter = rect.intersection(dRect)
                let area = inter.isNull ? 0 : (inter.width * inter.height)
                if area > bestArea {
                    bestArea = area
                    bestIdx = idx
                }
            }
            return bestIdx
        }

        var targets: [ScreenWindowTarget] = []
        var usedWindowIDs = Set<CGWindowID>()
        var coveredDisplays = Set<Int>()

        // 3. Resolve the exact focused window first (using AXFocusedWindow bounds when available so
        //    multi-window apps with windows on BOTH Screen 1 & Screen 2 pick the exact focused screen!).
        if frontPid > 0 && !frontBundle.hasPrefix("com.piya.swiftui") {
            let axFocusedBounds = AXIsProcessTrusted() ? focusedAxWindowBounds(pid: frontPid) : nil
            let matchedCG: ParsedCGWin? = {
                if let axRect = axFocusedBounds {
                    if let exact = onScreenWindows.first(where: {
                        $0.pid == frontPid && rectsApproximatelyMatch($0.bounds, axRect)
                    }) {
                        return exact
                    }
                }
                return onScreenWindows.first(where: { $0.pid == frontPid })
            }()

            let rect = matchedCG?.bounds ?? axFocusedBounds
            let dIdx = rect.map(displayIndexForRect) ?? 0
            if let wid = matchedCG?.winId {
                usedWindowIDs.insert(wid)
            }
            coveredDisplays.insert(dIdx)
            targets.append(
                ScreenWindowTarget(
                    pid: frontPid,
                    appName: frontName,
                    bundleId: frontBundle,
                    windowId: matchedCG?.winId,
                    cgTitle: matchedCG?.title ?? "",
                    bounds: rect,
                    displayIndex: dIdx,
                    totalDisplays: totalDisplays,
                    isFocused: true
                )
            )
        }

        // 4. For EVERY OTHER connected display, pick the topmost visible window on that screen!
        //    (e.g. Safari/Chrome/Terminal/PR open on LG FHD (2) while coding in Antigravity on LG FHD (1),
        //     OR a second window of the SAME app open on the second monitor!)
        if displays.count > 1 {
            for (dIdx, dRect) in displays.enumerated() where !coveredDisplays.contains(dIdx) {
                let displayArea = max(dRect.width * dRect.height, 1)
                guard let topOnDisplay = onScreenWindows.first(where: { win in
                    guard !usedWindowIDs.contains(win.winId) else { return false }
                    guard displayIndexForRect(win.bounds) == dIdx else { return false }
                    let inter = win.bounds.intersection(dRect)
                    guard !inter.isNull else { return false }
                    // Must occupy at least 10% of that monitor so tiny utility popovers don't count.
                    return (inter.width * inter.height) >= displayArea * 0.10
                }) else {
                    continue
                }

                usedWindowIDs.insert(topOnDisplay.winId)
                coveredDisplays.insert(dIdx)
                let (appName, bundleId) = runningAppsByPid[topOnDisplay.pid] ?? (topOnDisplay.ownerName, "")
                targets.append(
                    ScreenWindowTarget(
                        pid: topOnDisplay.pid,
                        appName: appName.isEmpty ? topOnDisplay.ownerName : appName,
                        bundleId: bundleId,
                        windowId: topOnDisplay.winId,
                        cgTitle: topOnDisplay.title,
                        bounds: topOnDisplay.bounds,
                        displayIndex: dIdx,
                        totalDisplays: totalDisplays,
                        isFocused: false
                    )
                )
            }
        }

        return targets
    }

    private nonisolated static func rectsApproximatelyMatch(_ a: CGRect, _ b: CGRect) -> Bool {
        abs(a.origin.x - b.origin.x) <= 40
            && abs(a.origin.y - b.origin.y) <= 60
            && abs(a.size.width - b.size.width) <= 60
            && abs(a.size.height - b.size.height) <= 80
    }

    private nonisolated static func focusedAxWindowBounds(pid: pid_t) -> CGRect? {
        let appElem = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(appElem, 0.2)
        guard let winRef = copyAxAttr(appElem, kAXFocusedWindowAttribute as String)
                ?? copyAxAttr(appElem, kAXMainWindowAttribute as String)
        else {
            return nil
        }
        return axWindowRect(winRef as! AXUIElement)
    }

    private nonisolated static func axWindowRect(_ win: AXUIElement) -> CGRect? {
        var posRef: CFTypeRef?
        var sizeRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(win, kAXPositionAttribute as CFString, &posRef) == .success,
              AXUIElementCopyAttributeValue(win, kAXSizeAttribute as CFString, &sizeRef) == .success,
              let posVal = posRef,
              let sizeVal = sizeRef
        else {
            return nil
        }
        var pt = CGPoint.zero
        var sz = CGSize.zero
        guard AXValueGetValue(posVal as! AXValue, .cgPoint, &pt),
              AXValueGetValue(sizeVal as! AXValue, .cgSize, &sz),
              sz.width > 0, sz.height > 0
        else {
            return nil
        }
        return CGRect(origin: pt, size: sz)
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
        target: ScreenWindowTarget,
        mode: String,
        allowOcr: Bool,
        forceOcr: Bool,
        idleSeconds: Double,
        ocrCache: [String: Date],
        sharedSCContent: SCShareableContent?
    ) async -> CapturedSnapshot {
        var axTitle = ""
        var urlOrPath = ""
        var axText = ""
        if AXIsProcessTrusted() {
            let ax = readAccessibilitySnapshot(
                pid: target.pid,
                bundleId: target.bundleId,
                targetBounds: target.bounds,
                preferFocusedWindow: target.isFocused,
                collectBodyText: mode != "app_only"
            )
            axTitle = ax.title
            urlOrPath = ax.urlOrPath
            axText = ax.bodyText
        }

        var finalTitle = !axTitle.isEmpty ? axTitle : target.cgTitle
        let windowKey = "\(target.bundleId)|\(target.windowId ?? 0)|\(target.displayIndex)|\(finalTitle)|\(urlOrPath)"

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

        var screenText = axText
        var captureKind = mode == "full" && allowOcr ? "full" : "text_only"
        var didRunOcr = false

        if allowOcr {
            let lastAt = ocrCache[windowKey]
            let elapsed = lastAt.map { Date().timeIntervalSince($0) } ?? .greatestFiniteMagnitude
            let neverOcredThisWindow = lastAt == nil
            let activelyWorking = idleSeconds <= activeInputWindowSeconds && elapsed >= activeOcrInterval
            let sparseAxRefresh = axText.count < 160 && elapsed >= 12.0

            let shouldRunOcr =
                forceOcr
                || neverOcredThisWindow
                || activelyWorking
                || sparseAxRefresh

            if shouldRunOcr {
                let scResult = await recognizeWindowTextViaVision(
                    pid: target.pid,
                    windowID: target.windowId,
                    windowBounds: target.bounds,
                    sharedContent: sharedSCContent
                )
                if !scResult.title.isEmpty && finalTitle.isEmpty {
                    finalTitle = scResult.title
                }
                if let ocrText = scResult.text, !ocrText.isEmpty {
                    didRunOcr = true
                    captureKind = "full"
                    screenText = combineAxAndOcrText(axText: axText, ocrText: ocrText)
                }
            }
        }

        // Tag display number when working across multiple monitors so Piya knows what is on Screen 1 vs Screen 2!
        if target.totalDisplays > 1 && !screenText.isEmpty && !screenText.hasPrefix("[Display ") {
            screenText = "[Display \(target.displayIndex + 1)]:\n" + screenText
        }

        return CapturedSnapshot(
            windowTitle: finalTitle,
            urlOrPath: urlOrPath,
            screenText: screenText,
            captureKind: captureKind,
            windowKey: "\(target.bundleId)|\(target.windowId ?? 0)|\(target.displayIndex)|\(finalTitle)|\(urlOrPath)",
            didRunOcr: didRunOcr
        )
    }

    private nonisolated static func combineAxAndOcrText(axText: String, ocrText: String) -> String {
        if axText.isEmpty {
            return "[Screen OCR]:\n" + ocrText
        }
        var seen = Set<String>()
        for line in axText.split(separator: "\n") {
            let clean = line.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if !clean.isEmpty {
                seen.insert(clean)
            }
        }
        var uniqueOcr: [String] = []
        for line in ocrText.split(separator: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.count >= 2 else { continue }
            if seen.insert(trimmed.lowercased()).inserted {
                uniqueOcr.append(trimmed)
            }
        }
        if uniqueOcr.isEmpty {
            return axText
        }
        return axText + "\n\n[Screen OCR]:\n" + uniqueOcr.joined(separator: "\n")
    }

    // MARK: - Accessibility reader (multi-window & secondary-display aware)

    private struct AxSnapshot {
        var title: String = ""
        var urlOrPath: String = ""
        var bodyText: String = ""
    }

    private nonisolated static func wakeElectronAccessibilityIfNeeded(appElem: AXUIElement, pid: pid_t, bundleId: String) {
        let b = bundleId.lowercased()
        let needsManualAx =
            b.contains("antigravity")
            || b.contains("todesktop")
            || b.contains("vscode")
            || b.contains("chrome")
            || b.contains("brave")
            || b.contains("thebrowser")
            || b.contains("chromium")
            || b.contains("microsoft.edgemac")
            || b.contains("slack")
            || b.contains("discord")
            || b.contains("notion")
            || b.contains("figma")
            || b.contains("linear")
            || b.contains("obsidian")
            || b.contains("nextcloud")
        guard needsManualAx else { return }

        awakenedAxPidsLock.lock()
        let already = awakenedAxPids.contains(pid)
        if !already {
            if awakenedAxPids.count > 256 {
                awakenedAxPids.removeAll()
            }
            awakenedAxPids.insert(pid)
        }
        awakenedAxPidsLock.unlock()

        if !already {
            _ = AXUIElementSetAttributeValue(appElem, "AXManualAccessibility" as CFString, kCFBooleanTrue)
            _ = AXUIElementSetAttributeValue(appElem, "AXEnhancedUserInterface" as CFString, kCFBooleanTrue)
        }
    }

    private nonisolated static func resolveTargetAxWindow(
        appElem: AXUIElement,
        targetBounds: CGRect?,
        preferFocusedWindow: Bool
    ) -> AXUIElement? {
        // When targeting a specific display's window (e.g. Display 2 or multi-window app),
        // match by Quartz window bounds across `kAXWindowsAttribute` first!
        if let targetBounds,
           let allWindows = copyAxChildren(appElem, kAXWindowsAttribute as String),
           !allWindows.isEmpty
        {
            if let matched = allWindows.first(where: { win in
                guard let r = axWindowRect(win) else { return false }
                return rectsApproximatelyMatch(r, targetBounds)
            }) {
                return matched
            }
        }
        if preferFocusedWindow {
            if let winRef = copyAxAttr(appElem, kAXFocusedWindowAttribute as String)
                ?? copyAxAttr(appElem, kAXMainWindowAttribute as String)
            {
                return (winRef as! AXUIElement)
            }
        }
        if let winRef = copyAxAttr(appElem, kAXMainWindowAttribute as String)
            ?? copyAxAttr(appElem, kAXFocusedWindowAttribute as String)
        {
            return (winRef as! AXUIElement)
        }
        return copyAxChildren(appElem, kAXWindowsAttribute as String)?.first
    }

    private nonisolated static func readAccessibilitySnapshot(
        pid: pid_t,
        bundleId: String,
        targetBounds: CGRect?,
        preferFocusedWindow: Bool,
        collectBodyText: Bool
    ) -> AxSnapshot {
        var out = AxSnapshot()
        let appElem = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(appElem, 0.25)
        wakeElectronAccessibilityIfNeeded(appElem: appElem, pid: pid, bundleId: bundleId)

        guard let win = resolveTargetAxWindow(
            appElem: appElem,
            targetBounds: targetBounds,
            preferFocusedWindow: preferFocusedWindow
        ) else {
            return out
        }
        AXUIElementSetMessagingTimeout(win, 0.25)

        if let t = copyAxString(win, kAXTitleAttribute as String) {
            out.title = t
        }
        if let doc = copyAxString(win, kAXDocumentAttribute as String), !doc.isEmpty {
            out.urlOrPath = cleanDocumentURL(doc)
        }

        var chunks: [String] = []
        var seen = Set<String>()
        func pushChunk(_ s: String, prefix: String? = nil) {
            let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.count >= 2 else { return }
            let clipped = String(trimmed.prefix(1_800))
            guard seen.insert(clipped).inserted else { return }
            if let prefix {
                chunks.append("\(prefix)\(clipped)")
            } else {
                chunks.append(clipped)
            }
        }

        // Only read app-wide focused element when inspecting the focused window itself
        // (otherwise a secondary window of the same app would inherit focused element from Screen 1!).
        if preferFocusedWindow, let focusedRef = copyAxAttr(appElem, kAXFocusedUIElementAttribute as String) {
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

        guard collectBodyText || out.urlOrPath.isEmpty else { return out }

        var queue: [(AXUIElement, Int)] = [(win, 0)]
        var visited = 0
        var charTotal = chunks.reduce(0) { $0 + $1.count }

        while !queue.isEmpty && visited < 320 && charTotal < 6_000 {
            let (elem, depth) = queue.removeFirst()
            visited += 1

            let role = copyAxString(elem, kAXRoleAttribute as String) ?? ""
            if role == "AXSecureTextField" {
                continue
            }
            if out.urlOrPath.isEmpty {
                if role == "AXWebArea", let u = copyAxString(elem, "AXURL") {
                    out.urlOrPath = cleanDocumentURL(u)
                } else if role == "AXTextField",
                          let desc = copyAxString(elem, kAXDescriptionAttribute as String)?.lowercased(),
                          desc.contains("address") || desc.contains("url") || desc.contains("search and enter"),
                          let val = copyAxString(elem, kAXValueAttribute as String),
                          !val.isEmpty
                {
                    out.urlOrPath = cleanDocumentURL(val)
                }
            }

            if collectBodyText {
                for attr in [kAXValueAttribute as String, kAXTitleAttribute as String, kAXDescriptionAttribute as String] {
                    if let s = copyAxString(elem, attr) {
                        let clean = s.trimmingCharacters(in: .whitespacesAndNewlines)
                        if clean.count >= 3 && clean != out.title {
                            let before = chunks.count
                            pushChunk(clean)
                            if chunks.count > before {
                                charTotal += Swift.min(clean.count, 1_800)
                            }
                        }
                    }
                }
            } else if !out.urlOrPath.isEmpty {
                break
            }

            guard depth < 10 else { continue }
            let children = copyAxChildren(elem, kAXVisibleChildrenAttribute as String)
                ?? copyAxChildren(elem, kAXChildrenAttribute as String)
                ?? []
            for child in children.prefix(40) {
                queue.append((child, depth + 1))
            }
        }

        if collectBodyText {
            out.bodyText = chunks.joined(separator: "\n")
        }
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

    // MARK: - Multi-Display ScreenCaptureKit + Resilient Apple Vision OCR

    private struct VisionCaptureResult: Sendable {
        var title: String
        var text: String?
    }

    private nonisolated static func recognizeWindowTextViaVision(
        pid: pid_t,
        windowID: CGWindowID?,
        windowBounds: CGRect?,
        sharedContent: SCShareableContent?
    ) async -> VisionCaptureResult {
        do {
            let content: SCShareableContent
            if let sharedContent {
                content = sharedContent
            } else {
                content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            }

            let scWindow: SCWindow? = {
                if let windowID, let exact = content.windows.first(where: { $0.windowID == windowID }) {
                    return exact
                }
                if let windowBounds {
                    if let byRect = content.windows.first(where: {
                        $0.owningApplication?.processID == pid
                            && $0.windowLayer == 0
                            && rectsApproximatelyMatch($0.frame, windowBounds)
                    }) {
                        return byRect
                    }
                }
                return content.windows
                    .filter {
                        $0.owningApplication?.processID == pid
                            && $0.windowLayer == 0
                            && $0.frame.width >= 160
                            && $0.frame.height >= 100
                    }
                    .max(by: { ($0.frame.width * $0.frame.height) < ($1.frame.width * $1.frame.height) })
            }()

            let scTitle = (scWindow?.title ?? "").trimmingCharacters(in: .whitespacesAndNewlines)

            // 1. Primary path: isolated desktop-independent window capture (even-pixel dimensions).
            if let scWindow {
                let filter = SCContentFilter(desktopIndependentWindow: scWindow)
                let config = SCStreamConfiguration()
                let w = Int((scWindow.frame.width * 2.0).clamped(to: 640...2560)) & ~1
                let h = Int((scWindow.frame.height * 2.0).clamped(to: 400...1600)) & ~1
                config.width = max(w, 640)
                config.height = max(h, 400)
                config.showsCursor = false

                if let cgImage = try? await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config),
                   let text = extractTextFromImage(cgImage),
                   !text.isEmpty
                {
                    return VisionCaptureResult(title: scTitle, text: text)
                }
            }

            // 2. Multi-display fallback: capture the specific `SCDisplay` (e.g. Display 2 at X=1920..3840)
            //    where `windowBounds` lives and crop to the window rectangle!
            let targetRect = windowBounds ?? scWindow?.frame
            if let targetRect,
               let scDisplay = content.displays.max(by: { a, b in
                   let ia = a.frame.intersection(targetRect)
                   let ib = b.frame.intersection(targetRect)
                   let areaA = ia.isNull ? 0 : ia.width * ia.height
                   let areaB = ib.isNull ? 0 : ib.width * ib.height
                   return areaA < areaB
               })
            {
                let excludedApps = content.applications.filter { app in
                    let bid = app.bundleIdentifier.lowercased()
                    return bid.hasPrefix("com.piya.swiftui")
                        || bid.hasPrefix("com.1password")
                        || bid.hasPrefix("com.apple.passwords")
                        || bid.hasPrefix("com.apple.keychainaccess")
                        || bid.hasPrefix("com.bitwarden")
                }
                let dFilter = SCContentFilter(
                    display: scDisplay,
                    excludingApplications: excludedApps,
                    exceptingWindows: []
                )
                let dConfig = SCStreamConfiguration()
                let scale: CGFloat = 2.0
                let dw = Int((scDisplay.frame.width * scale).clamped(to: 1280...3840)) & ~1
                let dh = Int((scDisplay.frame.height * scale).clamped(to: 720...2160)) & ~1
                dConfig.width = max(dw, 1280)
                dConfig.height = max(dh, 720)
                dConfig.showsCursor = false

                let fullDisplayImg = try await SCScreenshotManager.captureImage(
                    contentFilter: dFilter,
                    configuration: dConfig
                )
                let clipped = targetRect.intersection(scDisplay.frame)
                let scaleX = CGFloat(fullDisplayImg.width) / max(scDisplay.frame.width, 1)
                let scaleY = CGFloat(fullDisplayImg.height) / max(scDisplay.frame.height, 1)
                let cropRect = CGRect(
                    x: (clipped.origin.x - scDisplay.frame.origin.x) * scaleX,
                    y: (clipped.origin.y - scDisplay.frame.origin.y) * scaleY,
                    width: clipped.width * scaleX,
                    height: clipped.height * scaleY
                ).integral
                let finalImg = (!clipped.isNull && cropRect.width >= 120 && cropRect.height >= 80)
                    ? (fullDisplayImg.cropping(to: cropRect) ?? fullDisplayImg)
                    : fullDisplayImg

                if let text = extractTextFromImage(finalImg), !text.isEmpty {
                    return VisionCaptureResult(title: scTitle, text: text)
                }
            }

            return VisionCaptureResult(title: scTitle, text: nil)
        } catch {
            return VisionCaptureResult(title: "", text: nil)
        }
    }

    /// Runs Vision OCR with `.accurate` first, automatically falling back to `.fast`
    /// if the macOS Neural Engine (`e5rt`) rejects `.accurate` on this OS build.
    private nonisolated static func extractTextFromImage(_ cgImage: CGImage) -> String? {
        for level in [VNRequestTextRecognitionLevel.accurate, .fast] {
            let request = VNRecognizeTextRequest()
            request.recognitionLevel = level
            request.usesLanguageCorrection = true
            let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
            if (try? handler.perform([request])) != nil {
                let lines = (request.results ?? [])
                    .compactMap { $0.topCandidates(1).first?.string.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { $0.count >= 2 }
                if !lines.isEmpty {
                    return String(lines.joined(separator: "\n").prefix(7_500))
                }
            }
        }
        return nil
    }
}

private extension Comparable {
    func clamped(to limits: ClosedRange<Self>) -> Self {
        min(max(self, limits.lowerBound), limits.upperBound)
    }
}

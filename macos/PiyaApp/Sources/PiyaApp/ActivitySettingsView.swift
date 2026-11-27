import AppKit
import PiyaBindings
import SwiftUI

struct ActivitySettingsTab: View {
    @ObservedObject private var observer = ActivityObserver.shared

    @State private var retentionDays: UInt32 = 30
    @State private var excludedApps: [String] = []
    @State private var newExcludedApp = ""

    @State private var todayData = FfiActivityToday(
        day: "",
        totalActiveSec: 0,
        summary: "",
        topApps: [],
        journalEntries: [],
        recentEvents: []
    )
    @State private var searchQuery = ""
    @State private var searchResults: [FfiActivityEvent] = []
    @State private var isSearching = false
    @State private var isSyncingJournal = false
    @State private var status = ""

    private let modes: [(String, String, String)] = [
        (
            "full",
            "Full (Active Screen OCR + Window Text + Work Journal)",
            "Continuously tracks active apps, browser tabs/URLs, editor files, and accurate Retina Vision OCR — synthesizing concrete tasks & work threads locally."
        ),
        (
            "text_only",
            "Text Only (Apps + Accessibility Window Text)",
            "Tracks active apps, window titles, URLs/paths, and visible window text via macOS Accessibility — zero screenshots or OCR."
        ),
        (
            "app_only",
            "App Activity Only (Metadata & Time)",
            "Tracks only which app, window title, and URL/file path you're in and for how long — never reads window body text."
        ),
        (
            "off",
            "Off",
            "Disables all desktop activity tracking."
        ),
    ]

    private let retentionChoices: [(UInt32, String)] = [
        (7, "7 days (daily rollups kept)"),
        (30, "30 days (recommended; daily rollups kept)"),
        (90, "90 days (daily rollups kept)"),
        (0, "Keep detailed history indefinitely"),
    ]

    private var activeMode: String {
        observer.settings.mode
    }

    var body: some View {
        Form {
            captureModeSection
            permissionsSection
            workThreadsSection
            todaySummarySection
            timelineSection
            privacyAndRetentionSection
        }
        .formStyle(.grouped)
        .padding()
        .onAppear(perform: loadAll)
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            loadAll()
        }
    }

    // MARK: - Sections

    private var captureModeSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(modes, id: \.0) { id, title, subtitle in
                    let isSelected = activeMode == id
                    Button {
                        selectMode(id)
                    } label: {
                        HStack(alignment: .top, spacing: 10) {
                            Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                                .foregroundStyle(isSelected ? Color.accentColor : Color.secondary)
                                .font(.system(size: 15, weight: .semibold))
                                .padding(.top, 2)

                            VStack(alignment: .leading, spacing: 2) {
                                Text(title)
                                    .font(.callout.weight(isSelected ? .semibold : .regular))
                                    .foregroundStyle(.primary)
                                Text(subtitle)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            Spacer(minLength: 0)
                        }
                        .padding(.vertical, 6)
                        .padding(.horizontal, 8)
                        .contentShape(Rectangle())
                        .background(
                            RoundedRectangle(cornerRadius: 8)
                                .fill(isSelected ? Color.accentColor.opacity(0.10) : Color.clear)
                        )
                    }
                    .buttonStyle(.plain)
                }
            }

            HStack(spacing: 10) {
                if observer.settings.isPaused && activeMode != "off" {
                    Label("Paused", systemImage: "pause.circle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                    Button("Resume Now") {
                        observer.pause(minutes: 0)
                        status = "Activity tracking resumed."
                    }
                    .controlSize(.small)
                } else if observer.needsPermissionAttention {
                    Label(
                        activeMode == "full"
                            ? "Waiting for Screen Recording permission — OCR is currently blocked by macOS"
                            : "Waiting for Accessibility permission — window text is currently blocked",
                        systemImage: "exclamationmark.triangle.fill"
                    )
                    .font(.caption)
                    .foregroundStyle(.orange)
                } else if activeMode != "off" {
                    let charsBadge = observer.lastCapturedChars > 0
                        ? " · \(observer.lastCapturedChars) chars captured"
                        : ""
                    Label(
                        observer.lastObservedLabel.isEmpty
                            ? "Active (\(modeDisplayName(activeMode))) — watching foreground windows & OCR"
                            : "Last seen (\(modeDisplayName(activeMode))\(charsBadge)): \(observer.lastObservedLabel)",
                        systemImage: "record.circle"
                    )
                    .font(.caption)
                    .foregroundStyle(.green)
                    .lineLimit(1)

                    Spacer()

                    Button("Pause 1h") {
                        observer.pause(minutes: 60)
                        status = "Paused for 1 hour."
                    }
                    .controlSize(.small)
                }
            }
        } header: {
            Text("Desktop Activity & Work Context")
        } footer: {
            Text("All OCR text and synthesized work threads stay 100% local in data/piya.db. Screenshots are processed in memory by Apple Vision and discarded immediately.")
                .font(.caption)
        }
    }

    private var permissionsSection: some View {
        Section {
            HStack {
                Label(
                    observer.hasAccessibilityPermission
                        ? "Accessibility granted (window titles, browser URLs & Electron text)"
                        : "Accessibility not active for this build (needed for URLs & editor paths)",
                    systemImage: observer.hasAccessibilityPermission ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
                )
                .font(.callout)
                .foregroundStyle(observer.hasAccessibilityPermission ? .green : .orange)

                Spacer()

                if !observer.hasAccessibilityPermission {
                    Button("Grant Accessibility…") {
                        observer.requestAccessibilityPermission()
                    }
                    .controlSize(.small)
                }
            }

            if activeMode == "full" {
                HStack {
                    Label(
                        observer.hasScreenCapturePermission
                            ? "Screen Recording granted (Retina Vision OCR active)"
                            : "Screen Recording not active for this build (OCR blocked until granted & relaunched)",
                        systemImage: observer.hasScreenCapturePermission ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
                    )
                    .font(.callout)
                    .foregroundStyle(observer.hasScreenCapturePermission ? .green : .orange)

                    Spacer()

                    if !observer.hasScreenCapturePermission {
                        Button("Open Screen Recording…") {
                            observer.requestScreenCapturePermission()
                        }
                        .controlSize(.small)
                    }
                }
            }

            HStack(spacing: 10) {
                Button("Refresh Status") {
                    observer.refreshPermissions()
                    reloadTimeline()
                }
                .controlSize(.small)

                if !observer.hasAccessibilityPermission || (activeMode == "full" && !observer.hasScreenCapturePermission) {
                    Button("Relaunch Piya to Apply") {
                        observer.relaunchApp()
                    }
                    .controlSize(.small)
                }
            }
        } header: {
            Text("macOS Permissions")
        } footer: {
            if !observer.hasAccessibilityPermission || (activeMode == "full" && !observer.hasScreenCapturePermission) {
                Text("Clicking “Open Screen Recording…” or “Grant Accessibility…” automatically clears any stale signature from an older build so you only need to allow Piya once (then click “Relaunch Piya to Apply”).")
                    .font(.caption)
            }
        }
    }

    private var workThreadsSection: some View {
        Section {
            if todayData.journalEntries.isEmpty {
                Text(
                    observer.needsPermissionAttention
                        ? "Grant Screen Recording & Accessibility above so Piya can read active windows and synthesize your work threads."
                        : "No work threads synthesized yet today. Work across a few windows with OCR active, or click “Synthesize Now”."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            } else {
                ForEach(todayData.journalEntries, id: \.id) { item in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 6) {
                            Text("\(Self.formatTime(item.startedAt))–\(Self.formatTime(item.endedAt))")
                                .font(.system(.caption2, design: .monospaced))
                                .foregroundStyle(.secondary)
                            if !item.project.isEmpty {
                                Text(item.project)
                                    .font(.caption2.weight(.semibold))
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 1)
                                    .background(Color.accentColor.opacity(0.14), in: Capsule())
                            }
                            Spacer()
                            Text(Self.formatDuration(item.durationSec))
                                .font(.system(.caption2, design: .monospaced))
                                .foregroundStyle(.secondary)
                        }
                        Text(item.taskSummary)
                            .font(.callout.weight(.medium))
                        if !item.details.isEmpty {
                            Text(item.details)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        if !item.apps.isEmpty {
                            Text(item.apps.joined(separator: " · "))
                                .font(.caption2)
                                .foregroundStyle(.tertiary)
                        }
                    }
                    .padding(.vertical, 3)
                }
            }

            HStack(spacing: 10) {
                Button(isSyncingJournal ? "Synthesizing…" : "Synthesize Work Now") {
                    syncJournalNow()
                }
                .controlSize(.small)
                .disabled(isSyncingJournal)

                Button("Refresh") {
                    reloadTimeline()
                }
                .controlSize(.small)
            }
        } header: {
            Text("Active Work Threads Today")
        } footer: {
            Text("Synthesized automatically in the background (and whenever you ask Piya “what have I been working on?”) from your window titles, URLs, and on-device OCR.")
                .font(.caption)
        }
    }

    private var todaySummarySection: some View {
        Section("App Time Today (\(todayData.day.isEmpty ? "—" : todayData.day))") {
            if todayData.totalActiveSec == 0 {
                Text("No desktop activity recorded today yet. Switch between a couple of windows and click Refresh.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                LabeledContent("Active time tracked") {
                    Text(Self.formatDuration(todayData.totalActiveSec))
                        .font(.system(.body, design: .monospaced).weight(.semibold))
                }
                ForEach(todayData.topApps.prefix(6), id: \.appName) { stat in
                    HStack {
                        Text(stat.appName)
                            .font(.callout.weight(.medium))
                        if !stat.topWindow.isEmpty && stat.topWindow != stat.appName {
                            Text("· \(stat.topWindow)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                        Spacer()
                        Text(Self.formatDuration(stat.seconds))
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
    }

    private var timelineSection: some View {
        Section("Recent Window & OCR Timeline") {
            HStack {
                TextField("Search work threads, window titles, URLs, or screen OCR…", text: $searchQuery)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(runSearch)
                if !searchQuery.isEmpty {
                    Button("Clear") {
                        searchQuery = ""
                        isSearching = false
                        searchResults = []
                    }
                    .controlSize(.small)
                } else {
                    Button("Search") { runSearch() }
                        .controlSize(.small)
                        .disabled(searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }

            let items = isSearching ? searchResults : todayData.recentEvents
            if items.isEmpty {
                Text(isSearching ? "No matching activity events." : "No recent window events.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(items.prefix(30), id: \.id) { ev in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 8) {
                            Text(Self.formatTime(ev.startedAt))
                                .font(.system(.caption2, design: .monospaced))
                                .foregroundStyle(.secondary)
                            Text(ev.appName)
                                .font(.callout.weight(.medium))
                            Text(Self.captureBadge(ev))
                                .font(.caption2)
                                .foregroundStyle(ev.screenText.isEmpty && ev.captureMode != "app_only" ? .orange : .secondary)
                            Text(Self.formatDuration(ev.durationSec))
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 1)
                                .background(.quaternary, in: Capsule())
                            Spacer()
                            Button(role: .destructive) {
                                deleteEvent(ev.id)
                            } label: {
                                Image(systemName: "trash")
                                    .font(.caption)
                            }
                            .buttonStyle(.borderless)
                            .help("Delete this activity entry")
                        }
                        if !ev.windowTitle.isEmpty {
                            Text(ev.windowTitle)
                                .font(.caption)
                                .foregroundStyle(.primary)
                                .lineLimit(1)
                        }
                        if !ev.urlOrPath.isEmpty {
                            Text(ev.urlOrPath)
                                .font(.caption2)
                                .foregroundStyle(.tertiary)
                                .lineLimit(1)
                        }
                        if let snippet = Self.previewSnippet(ev.screenText) {
                            Text(snippet)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
        }
    }

    private var privacyAndRetentionSection: some View {
        Section {
            Picker("Detailed window history", selection: Binding(
                get: { retentionDays },
                set: { days in
                    retentionDays = days
                    persistSettings(mode: activeMode)
                }
            )) {
                ForEach(retentionChoices, id: \.0) { days, label in
                    Text(label).tag(days)
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Excluded apps (never recorded)")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if !excludedApps.isEmpty {
                    FlowExcludedTags(items: excludedApps) { removed in
                        excludedApps.removeAll { $0 == removed }
                        persistSettings(mode: activeMode)
                    }
                }

                HStack {
                    TextField("Add app name or bundle ID (e.g. Signal, com.tinyspeck.slackmacgap)", text: $newExcludedApp)
                        .textFieldStyle(.roundedBorder)
                        .onSubmit(addExcluded)
                    Button("Exclude") { addExcluded() }
                        .disabled(newExcludedApp.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }

            HStack(spacing: 12) {
                Button("Clear Today", role: .destructive) {
                    _ = try? PiyaBackend.clearActivity(todayOnly: true)
                    reloadTimeline()
                    status = "Cleared today's activity."
                }
                .controlSize(.small)

                Button("Clear All Activity…", role: .destructive) {
                    _ = try? PiyaBackend.clearActivity(todayOnly: false)
                    reloadTimeline()
                    status = "Cleared all recorded activity."
                }
                .controlSize(.small)

                Spacer()

                if !status.isEmpty {
                    Text(status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        } header: {
            Text("Privacy & Retention")
        } footer: {
            Text("1Password, Apple Passwords, Keychain Access, Bitwarden, password fields, Private/Incognito browser windows, API keys, and card numbers are blocked automatically.")
                .font(.caption)
        }
    }

    // MARK: - Helpers

    private func modeDisplayName(_ raw: String) -> String {
        switch raw {
        case "full": return "Full"
        case "app_only": return "App Only"
        default: return "Text Only"
        }
    }

    private static func captureBadge(_ ev: FfiActivityEvent) -> String {
        let count = ev.screenText.count
        switch ev.captureMode {
        case "full":
            return count > 0 ? "OCR · \(count) chars" : "Full (0 chars)"
        case "app_only":
            return "App"
        default:
            return count > 0 ? "Text · \(count) chars" : "No text"
        }
    }

    private static func previewSnippet(_ raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let lines = trimmed
            .components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { $0.count >= 4 && $0 != "[Screen OCR]:" && $0 != "[Earlier in window]:" }
            .prefix(2)
        guard !lines.isEmpty else { return nil }
        return lines.joined(separator: " · ")
    }

    private func selectMode(_ newMode: String) {
        persistSettings(mode: newMode)
    }

    private func loadAll() {
        observer.reloadSettings()
        observer.refreshPermissions()
        let s = observer.settings
        retentionDays = s.retentionDays
        excludedApps = s.excludedApps
        reloadTimeline()
    }

    private func reloadTimeline() {
        if let t = try? PiyaBackend.activityToday(limit: 40) {
            todayData = t
        }
    }

    private func syncJournalNow() {
        isSyncingJournal = true
        status = ""
        Task.detached(priority: .userInitiated) {
            let n = (try? PiyaBackend.syncActivityJournal(force: true)) ?? 0
            await MainActor.run {
                self.isSyncingJournal = false
                self.reloadTimeline()
                self.status = n > 0
                    ? "Synthesized \(n) work thread(s)."
                    : "No new titled/OCR windows to synthesize yet."
            }
        }
    }

    private func persistSettings(mode: String) {
        do {
            try observer.saveSettings(
                mode: mode,
                retentionDays: retentionDays,
                excludedApps: excludedApps
            )
            status = "Saved (\(modeDisplayName(mode)))."
        } catch {
            status = error.localizedDescription
        }
    }

    private func addExcluded() {
        let trimmed = newExcludedApp.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        if !excludedApps.contains(where: { $0.caseInsensitiveCompare(trimmed) == .orderedSame }) {
            excludedApps.append(trimmed)
            persistSettings(mode: activeMode)
        }
        newExcludedApp = ""
    }

    private func runSearch() {
        let q = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else {
            isSearching = false
            searchResults = []
            return
        }
        isSearching = true
        searchResults = (try? PiyaBackend.searchActivity(query: q, limit: 30)) ?? []
    }

    private func deleteEvent(_ id: Int64) {
        try? PiyaBackend.deleteActivityEvent(id: id)
        searchResults.removeAll { $0.id == id }
        reloadTimeline()
    }

    private static func formatDuration(_ sec: Int64) -> String {
        if sec < 60 { return "\(max(sec, 0))s" }
        if sec < 3600 { return "\(sec / 60)m" }
        let h = sec / 3600
        let m = (sec % 3600) / 60
        return m == 0 ? "\(h)h" : "\(h)h \(m)m"
    }

    private static func formatTime(_ ts: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ts))
        let f = DateFormatter()
        f.dateFormat = "HH:mm"
        return f.string(from: date)
    }
}

private struct FlowExcludedTags: View {
    let items: [String]
    let onRemove: (String) -> Void

    var body: some View {
        HStack(spacing: 6) {
            ForEach(items, id: \.self) { item in
                HStack(spacing: 4) {
                    Text(item)
                        .font(.caption)
                    Button {
                        onRemove(item)
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.caption2)
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(.quaternary, in: Capsule())
            }
        }
    }
}

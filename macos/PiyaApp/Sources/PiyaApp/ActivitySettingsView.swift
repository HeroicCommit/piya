import AppKit
import PiyaBindings
import SwiftUI

struct ActivitySettingsTab: View {
    @ObservedObject private var observer = ActivityObserver.shared

    @State private var mode = "text_only"
    @State private var retentionDays: UInt32 = 30
    @State private var excludedApps: [String] = []
    @State private var newExcludedApp = ""

    @State private var todayData = FfiActivityToday(
        day: "",
        totalActiveSec: 0,
        summary: "",
        topApps: [],
        recentEvents: []
    )
    @State private var searchQuery = ""
    @State private var searchResults: [FfiActivityEvent] = []
    @State private var isSearching = false
    @State private var status = ""

    private let modes: [(String, String, String)] = [
        (
            "full",
            "Full (Apps + Window Text + Screen OCR)",
            "Tracks active apps, window titles, URLs/paths, Accessibility text, plus on-device Apple Vision OCR for canvases/PDFs (screenshots are discarded immediately)."
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

    var body: some View {
        Form {
            captureModeSection
            permissionsSection
            privacyAndRetentionSection
            todaySummarySection
            timelineSection
        }
        .formStyle(.grouped)
        .padding()
        .onAppear(perform: loadAll)
    }

    // MARK: - Sections

    private var captureModeSection: some View {
        Section {
            Picker("Capture mode", selection: Binding(
                get: { mode },
                set: { newMode in
                    mode = newMode
                    persistSettings()
                    if newMode == "full" && !observer.hasScreenCapturePermission {
                        observer.requestScreenCapturePermission()
                    }
                }
            )) {
                ForEach(modes, id: \.0) { id, title, _ in
                    Text(title).tag(id)
                }
            }
            .pickerStyle(.radioGroup)

            if let desc = modes.first(where: { $0.0 == mode })?.2 {
                Text(desc)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            HStack(spacing: 10) {
                if observer.settings.isPaused && mode != "off" {
                    Label("Paused", systemImage: "pause.circle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                    Button("Resume Now") {
                        observer.pause(minutes: 0)
                        status = "Activity tracking resumed."
                    }
                    .controlSize(.small)
                } else if mode != "off" {
                    Label(
                        observer.lastObservedLabel.isEmpty
                            ? "Active — watching foreground window changes"
                            : "Last seen: \(observer.lastObservedLabel)",
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
            Text("All activity stays local in data/piya.db. Piya uses it to answer “what am I working on?” or “what did I do today?” without creating automatic tasks.")
                .font(.caption)
        }
    }

    private var permissionsSection: some View {
        Section("macOS Permissions") {
            HStack {
                Label(
                    observer.hasAccessibilityPermission
                        ? "Accessibility granted (window titles & text)"
                        : "Accessibility needed for window titles & Text Only / Full modes",
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

            if mode == "full" {
                HStack {
                    Label(
                        observer.hasScreenCapturePermission
                            ? "Screen Recording granted (on-device Vision OCR)"
                            : "Screen Recording needed for Full mode OCR",
                        systemImage: observer.hasScreenCapturePermission ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
                    )
                    .font(.callout)
                    .foregroundStyle(observer.hasScreenCapturePermission ? .green : .orange)

                    Spacer()

                    if !observer.hasScreenCapturePermission {
                        Button("Grant Screen Recording…") {
                            observer.requestScreenCapturePermission()
                        }
                        .controlSize(.small)
                    }
                }
            }

            Button("Refresh Permission Status") {
                observer.refreshPermissions()
            }
            .controlSize(.small)
        }
    }

    private var privacyAndRetentionSection: some View {
        Section {
            Picker("Detailed window history", selection: Binding(
                get: { retentionDays },
                set: { days in
                    retentionDays = days
                    persistSettings()
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
                        persistSettings()
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

    private var todaySummarySection: some View {
        Section("Today (\(todayData.day.isEmpty ? "—" : todayData.day))") {
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

            Button("Refresh") {
                reloadTimeline()
            }
            .controlSize(.small)
        }
    }

    private var timelineSection: some View {
        Section("Recent Window Timeline & Search") {
            HStack {
                TextField("Search window titles, URLs, or screen text…", text: $searchQuery)
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
                    }
                    .padding(.vertical, 2)
                }
            }
        }
    }

    // MARK: - Helpers

    private func loadAll() {
        observer.reloadSettings()
        observer.refreshPermissions()
        let s = observer.settings
        mode = s.mode
        retentionDays = s.retentionDays
        excludedApps = s.excludedApps
        reloadTimeline()
    }

    private func reloadTimeline() {
        if let t = try? PiyaBackend.activityToday(limit: 40) {
            todayData = t
        }
    }

    private func persistSettings() {
        do {
            try observer.saveSettings(
                mode: mode,
                retentionDays: retentionDays,
                excludedApps: excludedApps
            )
            status = "Saved."
        } catch {
            status = error.localizedDescription
        }
    }

    private func addExcluded() {
        let trimmed = newExcludedApp.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        if !excludedApps.contains(where: { $0.caseInsensitiveCompare(trimmed) == .orderedSame }) {
            excludedApps.append(trimmed)
            persistSettings()
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

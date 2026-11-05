import SwiftUI
import AppKit
import PiyaBindings

// MARK: - Models

private struct EditableMcpServer: Identifiable {
    let id: UUID
    var name: String
    var transport: String
    var command: String
    var url: String
    var args: String
    var env: String
    var headers: String
    var enabled: Bool

    init(_ s: FfiMcpServer, id: UUID = UUID()) {
        self.id = id
        name = s.name
        transport = s.transport.isEmpty ? (s.url.isEmpty ? "stdio" : "http") : s.transport
        command = s.command
        url = s.url
        args = s.args.joined(separator: "\n")
        env = s.env.map { "\($0.key)=\($0.value)" }.sorted().joined(separator: "\n")
        headers = s.headers.map { "\($0.key)=\($0.value)" }.sorted().joined(separator: "\n")
        enabled = s.enabled
    }

    static func blank() -> EditableMcpServer {
        EditableMcpServer(
            FfiMcpServer(
                name: "server",
                command: "",
                args: [],
                env: [:],
                enabled: true,
                transport: "stdio",
                url: "",
                headers: [:]
            )
        )
    }

    func toFfi() -> FfiMcpServer {
        FfiMcpServer(
            name: name.trimmingCharacters(in: .whitespacesAndNewlines),
            command: command,
            args: lines(args),
            env: kvMap(env),
            enabled: enabled,
            transport: transport,
            url: url.trimmingCharacters(in: .whitespacesAndNewlines),
            headers: kvMap(headers)
        )
    }
}

private func lines(_ text: String) -> [String] {
    text.split(separator: "\n")
        .map { $0.trimmingCharacters(in: .whitespaces) }
        .filter { !$0.isEmpty }
}

private func kvMap(_ text: String) -> [String: String] {
    var map: [String: String] = [:]
    for line in text.split(separator: "\n") {
        let parts = line.split(separator: "=", maxSplits: 1).map(String.init)
        if parts.count == 2 {
            map[parts[0].trimmingCharacters(in: .whitespaces)] = parts[1]
        }
    }
    return map
}

private struct MemoryRow: Identifiable {
    let id: String
    let text: String
    let kind: String
    let createdAt: String

    init(_ e: FfiMemoryEntry) {
        id = e.id
        text = e.text
        kind = e.kind
        createdAt = e.createdAt
    }
}

// MARK: - Root

struct SettingsView: View {
    var body: some View {
        TabView {
            GeneralSettingsTab()
                .tabItem { Label("General", systemImage: "gearshape") }
            YouSettingsTab()
                .tabItem { Label("You", systemImage: "person") }
            MemorySettingsTab()
                .tabItem { Label("Memory", systemImage: "brain") }
            NotificationsSettingsTab()
                .tabItem { Label("Notifications", systemImage: "bell") }
            ShellSettingsTab()
                .tabItem { Label("Shell", systemImage: "terminal") }
            McpSettingsTab()
                .tabItem { Label("MCP", systemImage: "puzzlepiece.extension") }
        }
        .frame(minWidth: 560, minHeight: 480)
    }
}

// MARK: - General

private enum KnownChatModels {
    static let options = [
        "gpt-4o-mini",
        "gpt-4o",
        "gpt-4.1-mini",
        "gpt-4.1",
        "o4-mini",
        "o3-mini",
        "gpt-4-turbo",
    ]
    static let customTag = "__custom__"
    static let `default` = "gpt-4o-mini"
}

private struct GeneralSettingsTab: View {
    @State private var version = ""
    @State private var dataDir = ""
    @State private var workspace = ""
    @State private var apiKey = ""
    @State private var baseUrl = ""
    @State private var modelSelection = KnownChatModels.default
    @State private var customModel = ""
    @State private var revealApiKey = false
    @State private var status = ""
    @State private var notify = "all"
    @State private var shellPolicy = "ask"

    private var resolvedModel: String {
        if modelSelection == KnownChatModels.customTag {
            return customModel.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return modelSelection
    }

    var body: some View {
        Form {
            Section("Piya") {
                LabeledContent("Version") {
                    Text(version.isEmpty ? "—" : version)
                        .foregroundStyle(.secondary)
                }
                pathRow("Data", dataDir)
                pathRow("Workspace", workspace)
            }

            Section("Model") {
                LabeledContent("OpenAI API key") {
                    HStack(spacing: 6) {
                        Group {
                            if revealApiKey {
                                TextField("sk-…", text: $apiKey)
                            } else {
                                SecureField("sk-…", text: $apiKey)
                            }
                        }
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 200)
                        .lineLimit(1)

                        Button {
                            revealApiKey.toggle()
                        } label: {
                            Image(systemName: revealApiKey ? "eye.slash" : "eye")
                        }
                        .buttonStyle(.borderless)
                        .help(revealApiKey ? "Hide key" : "Reveal key")
                        .accessibilityLabel(revealApiKey ? "Hide API key" : "Reveal API key")

                        Button {
                            apiKey = ""
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.borderless)
                        .disabled(apiKey.isEmpty)
                        .help("Clear key")
                        .accessibilityLabel("Clear API key")
                    }
                }

                TextField("Base URL", text: $baseUrl)

                Picker("Model", selection: $modelSelection) {
                    ForEach(KnownChatModels.options, id: \.self) { name in
                        Text(name).tag(name)
                    }
                    Divider()
                    Text("Custom…").tag(KnownChatModels.customTag)
                }
                if modelSelection == KnownChatModels.customTag {
                    TextField("Custom model id", text: $customModel)
                }
            }

            Section("Shortcuts") {
                LabeledContent("Ask Piya") {
                    Text("⌃⌘Space")
                        .foregroundStyle(.secondary)
                }
                Text("Works when Piya is focused. Global hotkey needs Accessibility permission (System Settings → Privacy & Security → Accessibility).")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Open Accessibility Settings…") {
                    if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") {
                        NSWorkspace.shared.open(url)
                    }
                }
            }

            if !status.isEmpty {
                Section {
                    Text(status).font(.caption).foregroundStyle(.secondary)
                }
            }

            HStack {
                Button("Reload") { load() }
                Spacer()
                Button("Save") { save() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .formStyle(.grouped)
        .padding()
        .onAppear(perform: load)
    }

    @ViewBuilder
    private func pathRow(_ label: String, _ path: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            LabeledContent(label) {
                Text(path.isEmpty ? "—" : path)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .lineLimit(2)
            }
            if !path.isEmpty {
                Button("Reveal in Finder") {
                    NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: path)
                }
                .font(.caption)
            }
        }
    }

    private func applyModelSelection(_ value: String) {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            modelSelection = KnownChatModels.default
            customModel = ""
        } else if KnownChatModels.options.contains(trimmed) {
            modelSelection = trimmed
            customModel = ""
        } else {
            modelSelection = KnownChatModels.customTag
            customModel = trimmed
        }
    }

    private func load() {
        version = PiyaBackend.version
        dataDir = PiyaBackend.dataDirectory
        workspace = (try? PiyaBackend.workspaceDirectory) ?? ""
        revealApiKey = false
        do {
            let env = try PiyaBackend.appEnvSettings()
            apiKey = env.openaiApiKey
            baseUrl = env.openaiBaseUrl
            applyModelSelection(env.model)
            notify = env.notify
            shellPolicy = env.shellPolicy
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    private func save() {
        do {
            // Preserve notify/shell from other tabs if General save runs alone.
            let current = (try? PiyaBackend.appEnvSettings())
            try PiyaBackend.saveAppEnvSettings(
                apiKey: apiKey,
                baseUrl: baseUrl,
                model: resolvedModel.isEmpty ? KnownChatModels.default : resolvedModel,
                notify: current?.notify ?? notify,
                shellPolicy: current?.shellPolicy ?? shellPolicy
            )
            status = "Saved — model settings apply on the next message."
        } catch {
            status = error.localizedDescription
        }
    }
}

// MARK: - You

private struct YouSettingsTab: View {
    @State private var profile = ""
    @State private var autoCapture = false
    @State private var digestEnabled = false
    @State private var digestHour: Double = 8
    @State private var status = ""

    var body: some View {
        Form {
            Section("About you") {
                Text("Pinned profile — injected into every chat turn so Piya remembers who you are.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                TextEditor(text: $profile)
                    .font(.body)
                    .frame(minHeight: 140)
            }

            Section("Learning") {
                Toggle("Auto-capture preferences from chat", isOn: $autoCapture)
                Text("When on, clear statements like “I prefer markdown” may be saved to memory automatically (opt-in).")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section("Digest (preview)") {
                Toggle("Morning digest", isOn: $digestEnabled)
                if digestEnabled {
                    Stepper(value: $digestHour, in: 0...23, step: 1) {
                        Text("Around \(Int(digestHour)):00")
                    }
                }
                Text("Scheduling isn’t wired yet — preference is saved for a future build.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if !status.isEmpty {
                Section {
                    Text(status).font(.caption).foregroundStyle(.secondary)
                }
            }

            HStack {
                Button("Reload") { load() }
                Spacer()
                Button("Save") { save() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .formStyle(.grouped)
        .padding()
        .onAppear(perform: load)
    }

    private func load() {
        do {
            let prefs = try PiyaBackend.userPrefs()
            profile = prefs.profile
            autoCapture = prefs.autoCapturePrefs
            digestEnabled = prefs.digestEnabled
            digestHour = Double(prefs.digestHour)
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    private func save() {
        do {
            try PiyaBackend.saveUserPrefs(
                profile: profile,
                autoCapture: autoCapture,
                digestEnabled: digestEnabled,
                digestHour: UInt32(digestHour)
            )
            status = "Saved — profile applies on the next message."
        } catch {
            status = error.localizedDescription
        }
    }
}

// MARK: - Memory

private struct MemorySettingsTab: View {
    @State private var memories: [MemoryRow] = []
    @State private var status = ""
    @State private var filter = ""

    var filtered: [MemoryRow] {
        let q = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return memories }
        return memories.filter {
            $0.text.lowercased().contains(q) || $0.kind.lowercased().contains(q)
        }
    }

    var body: some View {
        Form {
            Section {
                TextField("Search memories", text: $filter)
                if filtered.isEmpty {
                    Text(memories.isEmpty ? "No memories yet." : "No matches.")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(filtered) { item in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(item.kind)
                                    .font(.caption2)
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 2)
                                    .background(.quaternary, in: Capsule())
                                Spacer()
                                Button("Forget", role: .destructive) {
                                    forget(item.id)
                                }
                                .font(.caption)
                            }
                            Text(item.text)
                                .font(.callout)
                                .textSelection(.enabled)
                            if !item.createdAt.isEmpty {
                                Text(item.createdAt)
                                    .font(.caption2)
                                    .foregroundStyle(.tertiary)
                            }
                        }
                        .padding(.vertical, 4)
                    }
                }
            } header: {
                Text("Long-term memory")
            } footer: {
                Text("List and forget only — inline edit comes later.")
                    .font(.caption)
            }

            if !status.isEmpty {
                Section {
                    Text(status).font(.caption).foregroundStyle(.secondary)
                }
            }

            Button("Reload") { load() }
        }
        .formStyle(.grouped)
        .padding()
        .onAppear(perform: load)
    }

    private func load() {
        do {
            memories = try PiyaBackend.listMemories(limit: 200).map(MemoryRow.init)
            status = memories.isEmpty ? "" : "\(memories.count) memories"
        } catch {
            status = error.localizedDescription
            memories = []
        }
    }

    private func forget(_ id: String) {
        do {
            try PiyaBackend.deleteMemory(id: id)
            memories.removeAll { $0.id == id }
            status = "Forgot memory."
        } catch {
            status = error.localizedDescription
        }
    }
}

// MARK: - Notifications

private struct NotificationsSettingsTab: View {
    @State private var mode = "all"
    @State private var status = ""
    @State private var apiKey = ""
    @State private var baseUrl = ""
    @State private var model = ""
    @State private var shellPolicy = "ask"

    private let modes = [
        ("all", "Banners + alerts"),
        ("badge", "Quiet (no banners)"),
        ("0", "Off"),
    ]

    var body: some View {
        Form {
            Section("Notification Center") {
                Picker("Mode", selection: $mode) {
                    ForEach(modes, id: \.0) { value, label in
                        Text(label).tag(value)
                    }
                }
                .pickerStyle(.inline)
                Text("Maps to PIYA_NOTIFY. Task finish and chat alerts respect this setting.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Open Notifications Settings…") {
                    if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension") {
                        NSWorkspace.shared.open(url)
                    } else if let url = URL(string: "x-apple.systempreferences:com.apple.preference.notifications") {
                        NSWorkspace.shared.open(url)
                    }
                }
            }

            if !status.isEmpty {
                Section {
                    Text(status).font(.caption).foregroundStyle(.secondary)
                }
            }

            HStack {
                Button("Reload") { load() }
                Spacer()
                Button("Save") { save() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .formStyle(.grouped)
        .padding()
        .onAppear(perform: load)
    }

    private func load() {
        do {
            let env = try PiyaBackend.appEnvSettings()
            apiKey = env.openaiApiKey
            baseUrl = env.openaiBaseUrl
            model = env.model
            shellPolicy = env.shellPolicy
            let raw = env.notify.lowercased()
            if raw == "badge" {
                mode = "badge"
            } else if raw == "0" || raw == "off" || raw == "none" || raw == "false" {
                mode = "0"
            } else {
                mode = "all"
            }
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    private func save() {
        do {
            try PiyaBackend.saveAppEnvSettings(
                apiKey: apiKey,
                baseUrl: baseUrl,
                model: model,
                notify: mode,
                shellPolicy: shellPolicy
            )
            status = "Saved."
        } catch {
            status = error.localizedDescription
        }
    }
}

// MARK: - Shell

private struct ShellSettingsTab: View {
    @State private var policy = "ask"
    @State private var status = ""
    @State private var apiKey = ""
    @State private var baseUrl = ""
    @State private var model = ""
    @State private var notify = "all"

    private let policies = [
        ("ask", "Ask (Allow / Deny)"),
        ("allow", "Allow all"),
        ("deny", "Deny risky"),
    ]

    var body: some View {
        Form {
            Section("Command policy") {
                Picker("Outside auto-allow list", selection: $policy) {
                    ForEach(policies, id: \.0) { value, label in
                        Text(label).tag(value)
                    }
                }
                .pickerStyle(.inline)
                Text("Safe commands still run without a prompt. Risky ones show `$ cmd` and Ask / Allow / Deny. Maps to PIYA_SHELL_POLICY.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if !status.isEmpty {
                Section {
                    Text(status).font(.caption).foregroundStyle(.secondary)
                }
            }

            HStack {
                Button("Reload") { load() }
                Spacer()
                Button("Save") { save() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .formStyle(.grouped)
        .padding()
        .onAppear(perform: load)
    }

    private func load() {
        do {
            let env = try PiyaBackend.appEnvSettings()
            apiKey = env.openaiApiKey
            baseUrl = env.openaiBaseUrl
            model = env.model
            notify = env.notify
            let raw = env.shellPolicy.lowercased()
            if raw == "allow" || raw == "auto" || raw == "1" || raw == "true" {
                policy = "allow"
            } else if raw == "deny" || raw == "block" || raw == "0" || raw == "false" {
                policy = "deny"
            } else {
                policy = "ask"
            }
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    private func save() {
        do {
            try PiyaBackend.saveAppEnvSettings(
                apiKey: apiKey,
                baseUrl: baseUrl,
                model: model,
                notify: notify,
                shellPolicy: policy
            )
            status = "Saved — applies to the next shell command."
        } catch {
            status = error.localizedDescription
        }
    }
}

// MARK: - MCP

private struct McpSettingsTab: View {
    @State private var servers: [EditableMcpServer] = []
    @State private var status = ""
    @State private var dataDir = ""

    var body: some View {
        Form {
            Section("Data") {
                LabeledContent("Config directory") {
                    Text(dataDir)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
            }

            Section("MCP integrations") {
                if servers.isEmpty {
                    Text("No MCP servers configured.")
                        .foregroundStyle(.secondary)
                }
                ForEach($servers) { $server in
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            TextField("Name", text: $server.name)
                            Button("Remove", role: .destructive) {
                                servers.removeAll { $0.id == server.id }
                            }
                            .font(.caption)
                        }
                        Picker("Transport", selection: $server.transport) {
                            Text("stdio").tag("stdio")
                            Text("http").tag("http")
                        }
                        .pickerStyle(.segmented)
                        if server.transport == "http" {
                            TextField("URL", text: $server.url)
                            TextField("Headers (KEY=value)", text: $server.headers, axis: .vertical)
                                .lineLimit(2...4)
                        } else {
                            TextField("Command", text: $server.command)
                            TextField("Args (one per line)", text: $server.args, axis: .vertical)
                                .lineLimit(2...4)
                            TextField("Env (KEY=value)", text: $server.env, axis: .vertical)
                                .lineLimit(2...4)
                        }
                        Toggle("Enabled", isOn: $server.enabled)
                    }
                    .padding(.vertical, 4)
                }
                Button("Add server") {
                    var blank = EditableMcpServer.blank()
                    blank.name = "server\(servers.count + 1)"
                    servers.append(blank)
                }
            }

            if !status.isEmpty {
                Section {
                    Text(status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            HStack {
                Button("Reload") { load() }
                Spacer()
                Button("Save") { save() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .formStyle(.grouped)
        .padding()
        .onAppear(perform: load)
    }

    private func load() {
        dataDir = PiyaBackend.dataDirectory
        do {
            servers = try PiyaBackend.listMcpServers().map { EditableMcpServer($0) }
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    private func save() {
        do {
            try PiyaBackend.saveMcpServers(servers.map { $0.toFfi() })
            status = "Saved — tools apply on your next message."
        } catch {
            status = error.localizedDescription
        }
    }
}

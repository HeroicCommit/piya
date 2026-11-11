import SwiftUI
import AppKit
import PiyaBindings

// MARK: - Models

private struct KVPair: Identifiable {
    let id = UUID()
    var key: String
    var value: String
}

private struct ArgItem: Identifiable {
    let id = UUID()
    var value: String
}

private struct EditableMcpServer: Identifiable {
    let id: UUID
    var name: String
    var transport: String
    var command: String
    var url: String
    var args: [ArgItem]
    var env: [KVPair]
    var headers: [KVPair]
    var enabled: Bool

    init(_ s: FfiMcpServer, id: UUID = UUID()) {
        self.id = id
        name = s.name
        transport = s.transport.isEmpty ? (s.url.isEmpty ? "stdio" : "http") : s.transport
        command = s.command
        url = s.url
        args = s.args.map { ArgItem(value: $0) }
        env = s.env.sorted { $0.key < $1.key }.map { KVPair(key: $0.key, value: $0.value) }
        headers = s.headers.sorted { $0.key < $1.key }.map { KVPair(key: $0.key, value: $0.value) }
        enabled = s.enabled
    }

    static func blank(name: String = "server") -> EditableMcpServer {
        EditableMcpServer(
            FfiMcpServer(
                name: name,
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
            command: command.trimmingCharacters(in: .whitespacesAndNewlines),
            args: args.map { $0.value.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty },
            env: Self.map(env),
            enabled: enabled,
            transport: transport,
            url: url.trimmingCharacters(in: .whitespacesAndNewlines),
            headers: Self.map(headers)
        )
    }

    private static func map(_ pairs: [KVPair]) -> [String: String] {
        var out: [String: String] = [:]
        for p in pairs {
            let k = p.key.trimmingCharacters(in: .whitespaces)
            if !k.isEmpty { out[k] = p.value }
        }
        return out
    }

    /// Content fingerprint (ignores row ids) used to detect unsaved changes.
    var signature: String {
        let f = toFfi()
        let env = f.env.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }
        let hdr = f.headers.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }
        return [f.name, f.transport, f.command, f.url, f.args.joined(separator: "\u{1}"),
                env.joined(separator: "\u{1}"), hdr.joined(separator: "\u{1}"), "\(f.enabled)"]
            .joined(separator: "\u{2}")
    }

    /// Human-readable problems that block saving this server.
    func issues(among all: [EditableMcpServer]) -> [String] {
        var out: [String] = []
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            out.append("Name is required.")
        } else if all.contains(where: {
            $0.id != id && $0.name.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == trimmed.lowercased()
        }) {
            out.append("Another server already uses this name.")
        }
        if transport == "http" {
            let u = url.trimmingCharacters(in: .whitespacesAndNewlines)
            if let parsed = URL(string: u), let scheme = parsed.scheme?.lowercased(),
               ["http", "https"].contains(scheme), parsed.host != nil {
            } else {
                out.append("Enter a valid http(s) URL.")
            }
        } else if command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            out.append("Command is required.")
        }
        for (label, pairs) in [("variable", env), ("header", headers)] {
            let keys = pairs.map { $0.key.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
            if Set(keys).count != keys.count {
                out.append("Duplicate \(label) name.")
            }
        }
        return out
    }
}

private func looksSecret(_ key: String) -> Bool {
    let k = key.lowercased()
    return ["key", "token", "secret", "password", "passwd", "auth", "bearer", "credential"]
        .contains { k.contains($0) }
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
        .frame(minWidth: 760, minHeight: 540)
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

private enum ProbeState {
    case running
    case ok([FfiMcpToolInfo])
    case failed(String)
}

private struct McpPreset: Identifiable {
    let id: String
    let title: String
    let command: String
    let args: [String]

    static let all: [McpPreset] = [
        McpPreset(id: "filesystem", title: "Filesystem (npx)", command: "npx",
                  args: ["-y", "@modelcontextprotocol/server-filesystem", NSHomeDirectory()]),
        McpPreset(id: "fetch", title: "Fetch (uvx)", command: "uvx", args: ["mcp-server-fetch"]),
    ]
}

private struct McpSettingsTab: View {
    @State private var servers: [EditableMcpServer] = []
    @State private var savedSignature = ""
    @State private var selection: UUID?
    @State private var probes: [UUID: ProbeState] = [:]
    @State private var status = ""
    @State private var statusIsError = false
    @State private var showImport = false
    @State private var importText = ""
    @State private var importError = ""
    @State private var confirmRemove = false

    private var signature: String { servers.map(\.signature).joined(separator: "\u{3}") }
    private var isDirty: Bool { signature != savedSignature }
    private var hasIssues: Bool { servers.contains { !$0.issues(among: servers).isEmpty } }
    private var selectedIndex: Int? { servers.firstIndex { $0.id == selection } }

    var body: some View {
        VStack(spacing: 0) {
            HSplitView {
                sidebar
                    .frame(minWidth: 200, idealWidth: 220, maxWidth: 280)
                detail
                    .frame(minWidth: 400, maxWidth: .infinity)
            }
            Divider()
            footer
        }
        .onAppear(perform: load)
        .sheet(isPresented: $showImport) { importSheet }
        .confirmationDialog(
            "Remove “\(selectedIndex.map { servers[$0].name } ?? "server")”?",
            isPresented: $confirmRemove
        ) {
            Button("Remove", role: .destructive) { removeSelected() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Applied when you click Save. Revert brings it back.")
        }
    }

    // MARK: Sidebar

    private var sidebar: some View {
        VStack(spacing: 0) {
            List(selection: $selection) {
                ForEach($servers) { $server in
                    serverRow($server)
                        .tag(server.id)
                }
            }
            .listStyle(.sidebar)
            .overlay {
                if servers.isEmpty {
                    Text("No servers yet")
                        .foregroundStyle(.secondary)
                }
            }
            Divider()
            HStack(spacing: 4) {
                Menu {
                    Button("Blank server") { addBlank() }
                    Button("Paste JSON…") {
                        importText = ""
                        importError = ""
                        showImport = true
                    }
                    Divider()
                    ForEach(McpPreset.all) { preset in
                        Button(preset.title) { addPreset(preset) }
                    }
                } label: {
                    Image(systemName: "plus")
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .frame(width: 28)
                .help("Add a server")

                Button {
                    confirmRemove = true
                } label: {
                    Image(systemName: "minus")
                }
                .buttonStyle(.borderless)
                .disabled(selection == nil)
                .help("Remove the selected server")
                Spacer()
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
        }
    }

    private func serverRow(_ server: Binding<EditableMcpServer>) -> some View {
        let value = server.wrappedValue
        let problems = value.issues(among: servers)
        return HStack(spacing: 8) {
            statusDot(for: value)
            VStack(alignment: .leading, spacing: 1) {
                Text(value.name.isEmpty ? "Untitled" : value.name)
                    .lineLimit(1)
                Text(value.transport == "http" ? "HTTP" : "Command")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            if !problems.isEmpty {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                    .help(problems.joined(separator: "\n"))
            }
            Toggle("", isOn: server.enabled)
                .toggleStyle(.switch)
                .controlSize(.mini)
                .labelsHidden()
        }
        .opacity(value.enabled ? 1 : 0.55)
    }

    @ViewBuilder
    private func statusDot(for server: EditableMcpServer) -> some View {
        switch probes[server.id] {
        case .running:
            ProgressView().controlSize(.mini).frame(width: 10, height: 10)
        case .ok:
            Circle().fill(.green).frame(width: 9, height: 9).help("Connected")
        case .failed(let message):
            Circle().fill(.red).frame(width: 9, height: 9).help(message)
        case nil:
            Circle().strokeBorder(.secondary.opacity(0.6), lineWidth: 1).frame(width: 9, height: 9)
                .help("Not tested")
        }
    }

    // MARK: Detail

    @ViewBuilder
    private var detail: some View {
        if let index = selectedIndex {
            McpServerDetail(
                server: $servers[index],
                issues: servers[index].issues(among: servers),
                probe: probes[servers[index].id],
                onTest: { test(servers[index].id) }
            )
            .id(servers[index].id)
        } else {
            ContentUnavailableView {
                Label(servers.isEmpty ? "No MCP servers" : "No server selected",
                      systemImage: "puzzlepiece.extension")
            } description: {
                Text(servers.isEmpty
                     ? "MCP servers give Piya extra tools. Add one with + or paste a JSON config."
                     : "Pick a server on the left to edit it.")
            } actions: {
                if servers.isEmpty {
                    Button("Add server") { addBlank() }
                    Button("Paste JSON…") {
                        importText = ""
                        importError = ""
                        showImport = true
                    }
                }
            }
        }
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 10) {
            if isDirty {
                Label("Unsaved changes", systemImage: "circle.fill")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .labelStyle(.titleAndIcon)
            } else if !status.isEmpty {
                Text(status)
                    .font(.caption)
                    .foregroundStyle(statusIsError ? Color.red : Color.secondary)
                    .lineLimit(2)
            }
            if isDirty && hasIssues {
                Text("Fix the highlighted servers to save.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Revert") { load() }
                .disabled(!isDirty)
            Button("Save") { save() }
                .keyboardShortcut(.defaultAction)
                .disabled(!isDirty || hasIssues)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    // MARK: Actions

    private func load() {
        do {
            servers = try PiyaBackend.listMcpServers().map { EditableMcpServer($0) }
            savedSignature = signature
            probes = [:]
            selection = servers.first?.id
            status = ""
            statusIsError = false
        } catch {
            status = error.localizedDescription
            statusIsError = true
        }
    }

    private func save() {
        do {
            try PiyaBackend.saveMcpServers(servers.map { $0.toFfi() })
            savedSignature = signature
            status = "Saved — tools apply on your next message."
            statusIsError = false
        } catch {
            status = error.localizedDescription
            statusIsError = true
        }
    }

    private func uniqueName(_ base: String) -> String {
        let taken = Set(servers.map { $0.name.lowercased() })
        if !taken.contains(base.lowercased()) { return base }
        var n = 2
        while taken.contains("\(base) \(n)".lowercased()) { n += 1 }
        return "\(base) \(n)"
    }

    private func addBlank() {
        let server = EditableMcpServer.blank(name: uniqueName("server"))
        servers.append(server)
        selection = server.id
    }

    private func addPreset(_ preset: McpPreset) {
        var server = EditableMcpServer.blank(name: uniqueName(preset.id))
        server.command = preset.command
        server.args = preset.args.map { ArgItem(value: $0) }
        servers.append(server)
        selection = server.id
    }

    private func removeSelected() {
        guard let index = selectedIndex else { return }
        probes[servers[index].id] = nil
        servers.remove(at: index)
        selection = servers.indices.contains(index) ? servers[index].id : servers.last?.id
    }

    private func test(_ id: UUID) {
        guard let server = servers.first(where: { $0.id == id }) else { return }
        probes[id] = .running
        let ffi = server.toFfi()
        Task.detached {
            let result = PiyaBackend.testMcpServer(ffi)
            await MainActor.run {
                probes[id] = result.ok ? .ok(result.tools) : .failed(result.error)
            }
        }
    }

    // MARK: Import

    private var importSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Paste MCP JSON")
                .font(.headline)
            Text("Accepts a full config with an \"mcpServers\" object, a map of servers, or a single server with \"command\" or \"url\".")
                .font(.caption)
                .foregroundStyle(.secondary)
            TextEditor(text: $importText)
                .font(.system(.body, design: .monospaced))
                .frame(minWidth: 460, minHeight: 220)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
            if !importError.isEmpty {
                Text(importError)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            HStack {
                Spacer()
                Button("Cancel") { showImport = false }
                Button("Import") { runImport() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(importText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
    }

    private func runImport() {
        switch McpImport.parse(importText) {
        case .failure(let error):
            importError = error.message
        case .success(let parsed):
            var firstNew: UUID?
            for entry in parsed {
                var server = EditableMcpServer(entry)
                server.name = uniqueName(server.name)
                servers.append(server)
                firstNew = firstNew ?? server.id
            }
            selection = firstNew ?? selection
            showImport = false
        }
    }
}

private struct McpImportError: Error {
    let message: String
}

private enum McpImport {
    static func parse(_ text: String) -> Result<[FfiMcpServer], McpImportError> {
        guard let data = text.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            return .failure(McpImportError(message: "That isn't valid JSON (expected an object)."))
        }

        var entries: [(String, [String: Any])] = []
        if let map = root["mcpServers"] as? [String: Any] {
            entries = map.compactMap { k, v in (v as? [String: Any]).map { (k, $0) } }
        } else if root["command"] != nil || root["url"] != nil {
            entries = [("imported", root)]
        } else {
            entries = root.compactMap { k, v in
                guard let d = v as? [String: Any], d["command"] != nil || d["url"] != nil else { return nil }
                return (k, d)
            }
        }
        if entries.isEmpty {
            return .failure(McpImportError(message: "No servers found. Each server needs a \"command\" or \"url\"."))
        }
        entries.sort { $0.0.lowercased() < $1.0.lowercased() }

        return .success(entries.map { name, d in
            let url = (d["url"] as? String) ?? ""
            let type = (d["type"] as? String)?.lowercased()
            let transport = (type == "http" || type == "sse" || !url.isEmpty) ? "http" : "stdio"
            let enabled = (d["enabled"] as? Bool) ?? !((d["disabled"] as? Bool) ?? false)
            return FfiMcpServer(
                name: name,
                command: (d["command"] as? String) ?? "",
                args: (d["args"] as? [Any])?.map { "\($0)" } ?? [],
                env: stringMap(d["env"]),
                enabled: enabled,
                transport: transport,
                url: url,
                headers: stringMap(d["headers"])
            )
        })
    }

    private static func stringMap(_ value: Any?) -> [String: String] {
        guard let dict = value as? [String: Any] else { return [:] }
        return dict.mapValues { "\($0)" }
    }
}

// MARK: - MCP detail

private struct McpServerDetail: View {
    @Binding var server: EditableMcpServer
    let issues: [String]
    let probe: ProbeState?
    let onTest: () -> Void

    var body: some View {
        Form {
            if !issues.isEmpty {
                Section {
                    ForEach(issues, id: \.self) { issue in
                        Label(issue, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                            .font(.callout)
                    }
                }
            }

            Section("Server") {
                TextField("Name", text: $server.name)
                Toggle("Enabled", isOn: $server.enabled)
                Picker("Type", selection: $server.transport) {
                    Text("Command (stdio)").tag("stdio")
                    Text("HTTP").tag("http")
                }
                .pickerStyle(.segmented)
            }

            Section("Connection") {
                if server.transport == "http" {
                    TextField("URL", text: $server.url, prompt: Text("https://example.com/mcp"))
                        .textContentType(.URL)
                } else {
                    TextField("Command", text: $server.command, prompt: Text("npx"))
                    argsEditor
                }
            }

            if server.transport == "http" {
                Section("Headers") {
                    KeyValueEditor(pairs: $server.headers, namePlaceholder: "Header",
                                   emptyText: "No headers. Add an Authorization header for protected servers.")
                }
            } else {
                Section("Environment variables") {
                    KeyValueEditor(pairs: $server.env, namePlaceholder: "NAME",
                                   emptyText: "No variables.")
                }
            }

            Section("Connection test") {
                HStack {
                    Button {
                        onTest()
                    } label: {
                        Label("Test connection", systemImage: "bolt.horizontal")
                    }
                    .disabled(!issues.isEmpty || isRunning)
                    if isRunning {
                        ProgressView().controlSize(.small)
                    }
                    Spacer()
                }
                probeResult
            }
        }
        .formStyle(.grouped)
    }

    private var isRunning: Bool {
        if case .running = probe { return true }
        return false
    }

    @ViewBuilder
    private var probeResult: some View {
        switch probe {
        case .ok(let tools):
            Label(tools.isEmpty ? "Connected — no tools exposed" : "Connected — \(tools.count) tool\(tools.count == 1 ? "" : "s")",
                  systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
            if !tools.isEmpty {
                DisclosureGroup("Tools") {
                    ForEach(tools, id: \.name) { tool in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(tool.name).font(.system(.callout, design: .monospaced))
                            if !tool.description.isEmpty {
                                Text(tool.description)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(2)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                }
            }
        case .failed(let message):
            Label("Couldn't connect", systemImage: "xmark.octagon.fill")
                .foregroundStyle(.red)
            Text(message)
                .font(.caption)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        case .running, nil:
            EmptyView()
        }
    }

    private var argsEditor: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Arguments")
                .font(.subheadline)
                .foregroundStyle(.secondary)
            ForEach($server.args) { $arg in
                HStack {
                    TextField("Argument", text: $arg.value)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(.body, design: .monospaced))
                    Button {
                        server.args.removeAll { $0.id == arg.id }
                    } label: {
                        Image(systemName: "minus.circle")
                    }
                    .buttonStyle(.borderless)
                    .help("Remove argument")
                }
            }
            Button {
                server.args.append(ArgItem(value: ""))
            } label: {
                Label("Add argument", systemImage: "plus")
            }
            .buttonStyle(.borderless)
        }
    }
}

private struct KeyValueEditor: View {
    @Binding var pairs: [KVPair]
    let namePlaceholder: String
    let emptyText: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if pairs.isEmpty {
                Text(emptyText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            ForEach($pairs) { $pair in
                KeyValueRow(pair: $pair, namePlaceholder: namePlaceholder) {
                    pairs.removeAll { $0.id == pair.id }
                }
            }
            Button {
                pairs.append(KVPair(key: "", value: ""))
            } label: {
                Label("Add", systemImage: "plus")
            }
            .buttonStyle(.borderless)
        }
    }
}

private struct KeyValueRow: View {
    @Binding var pair: KVPair
    let namePlaceholder: String
    let onDelete: () -> Void
    @State private var reveal = false

    var body: some View {
        let secret = looksSecret(pair.key)
        HStack {
            TextField(namePlaceholder, text: $pair.key)
                .textFieldStyle(.roundedBorder)
                .frame(width: 150)
            Group {
                if secret && !reveal {
                    SecureField("Value", text: $pair.value)
                } else {
                    TextField("Value", text: $pair.value)
                }
            }
            .textFieldStyle(.roundedBorder)
            if secret {
                Button {
                    reveal.toggle()
                } label: {
                    Image(systemName: reveal ? "eye.slash" : "eye")
                }
                .buttonStyle(.borderless)
                .help(reveal ? "Hide value" : "Show value")
            }
            Button(role: .destructive, action: onDelete) {
                Image(systemName: "minus.circle")
            }
            .buttonStyle(.borderless)
            .help("Remove")
        }
    }
}

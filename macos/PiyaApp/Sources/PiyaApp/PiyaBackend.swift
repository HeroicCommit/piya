import Foundation
import PiyaBindings

enum AppPanel: String, CaseIterable {
    case chat
    case tasks

    var subtitle: String {
        switch self {
        case .chat: return "Ask me anything"
        case .tasks: return "Queue & schedules"
        }
    }

    var label: String {
        switch self {
        case .chat: return "Chat"
        case .tasks: return "Tasks"
        }
    }
}

/// Swift-facing wrapper around UniFFI-generated Rust calls.
enum PiyaBackend {
    private static var ready = false

    static func bootstrap() {
        guard !ready else { return }
        uniffiEnsurePiyaInitialized()
        _ = try? piyaBootstrapAppSupport()
        ready = true
    }

    static func send(message: String, history: [ChatMessage]) throws -> String {
        bootstrap()
        let turns = history.map {
            FfiChatTurn(role: $0.role, text: $0.text)
        }
        return try piyaSend(message: message, mode: .chat, history: turns)
    }

    /// Live status while a chat turn runs (e.g. `$ ls ~/Documents`). Empty when idle.
    static func activityStatus() -> String {
        bootstrap()
        return piyaActivityStatus()
    }

    static func cancelChat() {
        bootstrap()
        piyaCancelChat()
    }

    static func loadSession() throws -> [ChatMessage] {
        bootstrap()
        let session = try piyaLoadChatSession()
        let sessionId = (try? piyaActiveChatSessionId()) ?? "active"
        return session.messages.enumerated().map { index, turn in
            ChatMessage(
                role: turn.role,
                text: turn.text,
                id: ChatMessage.stableID(
                    sessionId: sessionId,
                    index: index,
                    role: turn.role,
                    text: turn.text
                )
            )
        }
    }

    static func saveSession(messages: [ChatMessage]) {
        bootstrap()
        let turns = messages.map {
            FfiChatTurn(role: $0.role, text: $0.text)
        }
        try? piyaSaveChatSession(mode: "chat", messages: turns)
    }

    static func listSessions() throws -> [ChatSessionItem] {
        bootstrap()
        return try piyaListChatSessions().map(ChatSessionItem.init)
    }

    static func activeSessionId() throws -> String {
        bootstrap()
        return try piyaActiveChatSessionId()
    }

    static func selectSession(id: String) throws {
        bootstrap()
        try piyaSelectChatSession(id: id)
    }

    static func createSession() throws -> ChatSessionItem {
        bootstrap()
        let meta = try piyaCreateChatSession(mode: "chat")
        return ChatSessionItem(meta)
    }

    static func deleteSession(id: String) throws {
        bootstrap()
        try piyaDeleteChatSession(id: id)
    }

    static func listTasks() throws -> [TaskItem] {
        bootstrap()
        return try piyaListTasks().map(TaskItem.init)
    }

    /// Start the CLI worker when queued tasks are due (no-op if already running).
    static func runPendingTasks() {
        bootstrap()
        try? piyaRunPendingTasks()
    }

    static func cancelTask(id: String) throws {
        bootstrap()
        try piyaCancelTask(id: id)
    }

    static func updateTask(id: String, title: String, description: String) throws {
        bootstrap()
        try piyaUpdateTask(id: id, title: title, description: description)
    }

    static func listMcpServers() throws -> [FfiMcpServer] {
        bootstrap()
        return try piyaListMcpServers()
    }

    static func saveMcpServers(_ servers: [FfiMcpServer]) throws {
        bootstrap()
        try piyaSaveMcpServers(servers: servers)
    }

    static var version: String {
        bootstrap()
        return piyaVersion()
    }

    static var dataDirectory: String {
        bootstrap()
        return (try? piyaDataDir()) ?? "unknown"
    }

    static var workspaceDirectory: String {
        get throws {
            bootstrap()
            return try piyaWorkspaceDir()
        }
    }

    static func userPrefs() throws -> FfiUserPrefs {
        bootstrap()
        return try piyaGetUserPrefs()
    }

    static func saveUserPrefs(
        profile: String,
        autoCapture: Bool,
        digestEnabled: Bool,
        digestHour: UInt32
    ) throws {
        bootstrap()
        try piyaSaveUserPrefs(
            prefs: FfiUserPrefs(
                profile: profile,
                autoCapturePrefs: autoCapture,
                digestEnabled: digestEnabled,
                digestHour: digestHour
            )
        )
    }

    static func appEnvSettings() throws -> FfiAppEnvSettings {
        bootstrap()
        return try piyaGetAppEnvSettings()
    }

    static func saveAppEnvSettings(
        apiKey: String,
        baseUrl: String,
        model: String,
        notify: String,
        shellPolicy: String
    ) throws {
        bootstrap()
        try piyaSaveAppEnvSettings(
            env: FfiAppEnvSettings(
                openaiApiKey: apiKey,
                openaiBaseUrl: baseUrl,
                model: model,
                notify: notify,
                shellPolicy: shellPolicy
            )
        )
    }

    static func listMemories(limit: UInt32) throws -> [FfiMemoryEntry] {
        bootstrap()
        return try piyaListMemories(limit: limit)
    }

    static func deleteMemory(id: String) throws {
        bootstrap()
        try piyaDeleteMemory(id: id)
    }
}

struct ChatMessage: Identifiable, Equatable {
    let id: UUID
    let role: String
    let text: String

    init(role: String, text: String, id: UUID = UUID()) {
        self.id = id
        self.role = role
        self.text = text
    }

    /// Stable id so scroll position survives reload of the same turn.
    static func stableID(sessionId: String, index: Int, role: String, text: String) -> UUID {
        var hasher = Hasher()
        hasher.combine(sessionId)
        hasher.combine(index)
        hasher.combine(role)
        hasher.combine(text.utf16.count)
        hasher.combine(text.prefix(64))
        hasher.combine(text.suffix(64))
        let bits = UInt64(bitPattern: Int64(hasher.finalize()))
        return UUID(uuid: (
            UInt8(truncatingIfNeeded: bits >> 56),
            UInt8(truncatingIfNeeded: bits >> 48),
            UInt8(truncatingIfNeeded: bits >> 40),
            UInt8(truncatingIfNeeded: bits >> 32),
            UInt8(truncatingIfNeeded: bits >> 24),
            UInt8(truncatingIfNeeded: bits >> 16),
            UInt8(truncatingIfNeeded: bits >> 8),
            UInt8(truncatingIfNeeded: bits),
            0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18
        ))
    }
}

struct TaskItem: Identifiable, Equatable, Hashable {
    let id: String
    let title: String
    let description: String
    let status: String
    let category: TaskCategory
    let scheduleLabel: String
    let runAt: String?
    let isRecurring: Bool

    init(_ ffi: FfiTaskItem) {
        id = ffi.id
        title = ffi.title
        description = ffi.description
        status = ffi.status
        category = TaskCategory(ffi.category)
        scheduleLabel = ffi.scheduleLabel
        runAt = ffi.runAt
        isRecurring = ffi.isRecurring
    }
}

enum TaskCategory: String, CaseIterable {
    case running
    case scheduled
    case queued

    init(_ raw: String) {
        self = TaskCategory(rawValue: raw) ?? .queued
    }

    var title: String {
        switch self {
        case .running: return "Running"
        case .scheduled: return "Scheduled"
        case .queued: return "Queued"
        }
    }
}

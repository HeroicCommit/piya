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

    static func send(
        message: String,
        attachments: [AttachmentItem] = [],
        history: [ChatMessage]
    ) throws -> String {
        bootstrap()
        let turns = history.map {
            FfiChatTurn(
                role: $0.role,
                text: $0.text,
                attachments: $0.attachments.map(\.ffi)
            )
        }
        return try piyaSendWithAttachments(
            message: message,
            attachments: attachments.map(\.ffi),
            mode: .chat,
            history: turns
        )
    }

    static func stageAttachment(
        sessionId: String,
        sourcePath: String,
        displayName: String = "",
        extractedText: String = ""
    ) throws -> AttachmentItem {
        bootstrap()
        let ffi = try piyaStageAttachment(
            sessionId: sessionId,
            sourcePath: sourcePath,
            displayName: displayName,
            extractedText: extractedText
        )
        return AttachmentItem(ffi)
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
            let atts = turn.attachments.map(AttachmentItem.init)
            return ChatMessage(
                role: turn.role,
                text: turn.text,
                attachments: atts,
                id: ChatMessage.stableID(
                    sessionId: sessionId,
                    index: index,
                    role: turn.role,
                    text: turn.text,
                    attachmentIds: atts.map(\.id)
                )
            )
        }
    }

    static func saveSession(messages: [ChatMessage]) {
        bootstrap()
        let turns = messages.map {
            FfiChatTurn(
                role: $0.role,
                text: $0.text,
                attachments: $0.attachments.map(\.ffi)
            )
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

    /// Start the server, run the MCP handshake and list its tools (blocking, up to ~15s).
    static func testMcpServer(_ server: FfiMcpServer) -> FfiMcpProbe {
        bootstrap()
        return piyaTestMcpServer(server: server)
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
        learnFromConversations: Bool = true,
        rememberSensitive: Bool = true,
        digestEnabled: Bool,
        digestHour: UInt32
    ) throws {
        bootstrap()
        try piyaSaveUserPrefs(
            prefs: FfiUserPrefs(
                profile: profile,
                autoCapturePrefs: autoCapture,
                learnFromConversations: learnFromConversations,
                rememberSensitive: rememberSensitive,
                digestEnabled: digestEnabled,
                digestHour: digestHour
            )
        )
    }

    static func saveLearningPrefs(
        learnFromConversations: Bool,
        rememberSensitive: Bool
    ) throws {
        bootstrap()
        var current = try piyaGetUserPrefs()
        current.learnFromConversations = learnFromConversations
        current.rememberSensitive = rememberSensitive
        try piyaSaveUserPrefs(prefs: current)
    }

    static func appEnvSettings() throws -> FfiAppEnvSettings {
        bootstrap()
        return try piyaGetAppEnvSettings()
    }

    static func saveAppEnvSettings(notify: String, shellPolicy: String) throws {
        bootstrap()
        try piyaSaveAppEnvSettings(env: FfiAppEnvSettings(notify: notify, shellPolicy: shellPolicy))
    }

    // MARK: Major alerts

    static func snoozeAlert(title: String, body: String, minutes: UInt32) throws -> String {
        bootstrap()
        return try piyaSnoozeAlert(title: title, body: body, minutes: minutes)
    }

    static func testMajorAlert() {
        bootstrap()
        piyaTestMajorAlert()
    }

    // MARK: Models

    static func providers() -> [FfiProvider] {
        bootstrap()
        return piyaListProviders()
    }

    static func modelSettings() throws -> FfiModelSettings {
        bootstrap()
        return try piyaGetModelSettings()
    }

    static func saveModelSettings(_ settings: FfiModelSettings) throws {
        bootstrap()
        try piyaSaveModelSettings(settings: settings)
    }

    static func listModels(provider: String, apiKey: String, baseUrl: String, embeddings: Bool) throws -> [String] {
        bootstrap()
        return try piyaListModels(provider: provider, apiKey: apiKey, baseUrl: baseUrl, embeddings: embeddings)
    }

    static func testModel(provider: String, apiKey: String, baseUrl: String, model: String, embeddings: Bool) -> FfiModelProbe {
        bootstrap()
        return piyaTestModel(provider: provider, apiKey: apiKey, baseUrl: baseUrl, model: model, embeddings: embeddings)
    }

    static func keyWarning(provider: String, apiKey: String) -> String {
        bootstrap()
        return piyaKeyWarning(provider: provider, apiKey: apiKey)
    }

    static func listMemories(limit: UInt32) throws -> [FfiMemoryEntry] {
        bootstrap()
        return try piyaListMemories(limit: limit)
    }

    static func deleteMemory(id: String) throws {
        bootstrap()
        try piyaDeleteMemory(id: id)
    }

    // MARK: Structured Notes & Learned Skills

    static func listNotes(includeArchived: Bool = false) throws -> [FfiNote] {
        bootstrap()
        return try piyaListNotes(includeArchived: includeArchived)
    }

    static func saveNote(id: String = "", area: String, slug: String = "", text: String) throws -> FfiNote {
        bootstrap()
        return try piyaSaveNote(id: id, area: area, slug: slug, text: text)
    }

    static func confirmNote(id: String) throws -> FfiNote {
        bootstrap()
        return try piyaConfirmNote(id: id)
    }

    static func forgetNote(id: String) throws {
        bootstrap()
        try piyaForgetNote(id: id)
    }

    static func listSkills() throws -> [FfiLearnedSkill] {
        bootstrap()
        return try piyaListSkills()
    }

    static func saveSkill(name: String, description: String, body: String) throws -> FfiLearnedSkill {
        bootstrap()
        return try piyaSaveSkill(name: name, description: description, body: body)
    }

    static func deleteSkill(name: String) throws {
        bootstrap()
        try piyaDeleteSkill(name: name)
    }

    static func runReflection(force: Bool = true) throws -> String {
        bootstrap()
        return try piyaRunReflection(force: force)
    }

    static func exportMemoryMarkdown() throws -> String {
        bootstrap()
        return try piyaExportMemoryMarkdown()
    }

    // MARK: Desktop Activity Tracking

    static func activitySettings() throws -> FfiActivitySettings {
        bootstrap()
        return try piyaGetActivitySettings()
    }

    static func saveActivitySettings(
        mode: String,
        retentionDays: UInt32,
        excludedApps: [String]
    ) throws -> FfiActivitySettings {
        bootstrap()
        return try piyaSaveActivitySettings(
            mode: mode,
            retentionDays: retentionDays,
            excludedApps: excludedApps
        )
    }

    static func pauseActivity(minutes: UInt32) throws -> FfiActivitySettings {
        bootstrap()
        return try piyaPauseActivity(minutes: minutes)
    }

    static func recordActivity(
        appName: String,
        bundleId: String,
        windowTitle: String,
        urlOrPath: String,
        screenText: String,
        captureKind: String,
        isFocused: Bool = true
    ) throws -> Bool {
        bootstrap()
        return try piyaRecordActivity(
            appName: appName,
            bundleId: bundleId,
            windowTitle: windowTitle,
            urlOrPath: urlOrPath,
            screenText: screenText,
            captureKind: captureKind,
            isFocused: isFocused
        )
    }

    static func activityToday(limit: UInt32 = 40) throws -> FfiActivityToday {
        bootstrap()
        return try piyaGetActivityToday(limit: limit)
    }

    static func syncActivityJournal(force: Bool = false) throws -> UInt32 {
        bootstrap()
        return try piyaSyncActivityJournal(force: force)
    }

    static func searchActivity(query: String, limit: UInt32 = 30) throws -> [FfiActivityEvent] {
        bootstrap()
        return try piyaSearchActivity(query: query, limit: limit)
    }

    static func deleteActivityEvent(id: Int64) throws {
        bootstrap()
        try piyaDeleteActivityEvent(id: id)
    }

    static func clearActivity(todayOnly: Bool) throws -> UInt32 {
        bootstrap()
        return try piyaClearActivity(todayOnly: todayOnly)
    }

    static func checkForUpdate() throws -> FfiUpdateCheck {
        bootstrap()
        return try piyaCheckForUpdate()
    }

    static func installUpdateAndRelaunch() throws -> String {
        bootstrap()
        return try piyaInstallUpdateAndRelaunch()
    }
}

struct AttachmentItem: Identifiable, Equatable {
    let id: String
    let name: String
    let path: String
    let resolvedPath: String
    let mime: String
    let kind: String
    let text: String

    init(_ ffi: FfiAttachment) {
        id = ffi.id
        name = ffi.name
        path = ffi.path
        resolvedPath = ffi.resolvedPath
        mime = ffi.mime
        kind = ffi.kind
        text = ffi.text
    }

    var ffi: FfiAttachment {
        FfiAttachment(
            id: id,
            name: name,
            path: path,
            resolvedPath: resolvedPath,
            mime: mime,
            kind: kind,
            text: text
        )
    }

    var isImage: Bool { kind == "image" }

    var badgeLabel: String {
        switch kind {
        case "image": return "Image"
        case "pdf": return text.isEmpty ? "PDF" : "PDF · text ready"
        case "audio": return text.isEmpty ? "Audio" : "Voice · transcribed"
        case "document": return "Document"
        default: return "File"
        }
    }

    var symbolName: String {
        switch kind {
        case "image": return "photo"
        case "pdf": return "doc.richtext"
        case "audio": return "waveform"
        case "document": return "doc.text"
        default: return "paperclip"
        }
    }
}

struct ChatMessage: Identifiable, Equatable {
    let id: UUID
    let role: String
    let text: String
    let attachments: [AttachmentItem]

    init(
        role: String,
        text: String,
        attachments: [AttachmentItem] = [],
        id: UUID = UUID()
    ) {
        self.id = id
        self.role = role
        self.text = text
        self.attachments = attachments
    }

    /// Stable id so scroll position survives reload of the same turn.
    static func stableID(
        sessionId: String,
        index: Int,
        role: String,
        text: String,
        attachmentIds: [String] = []
    ) -> UUID {
        var hasher = Hasher()
        hasher.combine(sessionId)
        hasher.combine(index)
        hasher.combine(role)
        hasher.combine(text.utf16.count)
        hasher.combine(text.prefix(64))
        hasher.combine(text.suffix(64))
        for aid in attachmentIds {
            hasher.combine(aid)
        }
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

import AppKit
import Foundation
import PiyaBindings

@MainActor
final class ChatViewModel: ObservableObject {
    @Published var panel: AppPanel = .chat
    @Published var messages: [ChatMessage] = []
    @Published var sessions: [ChatSessionItem] = []
    @Published var tasks: [TaskItem] = []
    @Published var activeSessionId = ""
    @Published var showSidebar = true
    @Published var input = ""
    /// Full text of a large paste kept out of the TextField to avoid layout thrash.
    @Published var largePaste: String?
    @Published var isSending = false
    @Published var activityStatus: String?
    @Published var errorMessage: String?
    @Published var editingTask: TaskItem?
    /// Bumped when the active session's messages are replaced so the list can settle scroll.
    @Published private(set) var sessionContentEpoch: UInt64 = 0
    /// Invalidates a late reply if send times out, is stopped, or a newer send starts.
    private var sendGeneration: UInt64 = 0
    private var statusPoll: Task<Void, Never>?
    private var timeoutTask: Task<Void, Never>?
    private static let sendTimeoutSeconds: Double = 180

    /// Text currently queued to send (large paste chip or normal input).
    var outgoingText: String {
        if let largePaste, !largePaste.isEmpty { return largePaste }
        return input
    }

    var hasOutgoingText: Bool {
        !outgoingText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Move oversized TextField content into the compact paste chip.
    func absorbLargeInputIfNeeded() {
        guard largePaste == nil, input.count > MessageMarkdown.composerCompactLimit else { return }
        largePaste = input
        input = ""
    }

    func clearComposer() {
        input = ""
        largePaste = nil
    }

    func attachPasteFromClipboard() {
        guard let text = NSPasteboard.general.string(forType: .string), !text.isEmpty else { return }
        if text.count > MessageMarkdown.composerCompactLimit {
            largePaste = text
            input = ""
        } else if input.isEmpty {
            input = text
        } else {
            input += text
            absorbLargeInputIfNeeded()
        }
    }

    init() {
        reloadSessions(selectActive: true)
        reloadTasks()
    }

    func reloadSessions(selectActive: Bool) {
        do {
            sessions = try PiyaBackend.listSessions()
            if selectActive || activeSessionId.isEmpty {
                activeSessionId = try PiyaBackend.activeSessionId()
            }
            loadActiveSessionContent(markEpoch: selectActive)
        } catch {
            errorMessage = error.localizedDescription
            resetWelcome()
        }
    }

    func reloadTasks() {
        PiyaBackend.runPendingTasks()
        if let list = try? PiyaBackend.listTasks() {
            tasks = list
        }
    }

    private func loadActiveSessionContent(markEpoch: Bool = true) {
        guard !activeSessionId.isEmpty else {
            resetWelcome()
            return
        }
        if let messages = try? PiyaBackend.loadSession(), !messages.isEmpty {
            self.messages = messages
        } else {
            resetWelcome()
        }
        if markEpoch {
            sessionContentEpoch &+= 1
        }
    }

    func persistSession() {
        PiyaBackend.saveSession(messages: messages)
        refreshSessionList()
    }

    /// Write only when leaving a session that may have unsaved edits.
    /// Backend ignores no-op saves for `updated_at`; this avoids needless I/O.
    private func persistSessionIfNeeded() {
        persistSession()
    }

    private func refreshSessionList() {
        if let list = try? PiyaBackend.listSessions() {
            sessions = list
        }
    }

    func selectSession(_ id: String) {
        guard id != activeSessionId, !isSending else { return }
        // Persist current chat only if content changed — selection alone must not
        // bump "last updated" or reshuffle the sidebar.
        persistSessionIfNeeded()
        do {
            try PiyaBackend.selectSession(id: id)
            activeSessionId = id
            loadActiveSessionContent(markEpoch: true)
            refreshSessionList()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func newSession() {
        guard !isSending else { return }
        persistSessionIfNeeded()
        do {
            let meta = try PiyaBackend.createSession()
            activeSessionId = meta.id
            resetWelcome()
            sessionContentEpoch &+= 1
            reloadSessions(selectActive: false)
            persistSession()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func deleteSession(_ id: String) {
        guard sessions.count > 1 else { return }
        do {
            let switching = id == activeSessionId
            try PiyaBackend.deleteSession(id: id)
            reloadSessions(selectActive: switching)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func setPanel(_ newPanel: AppPanel) {
        panel = newPanel
        if newPanel == .tasks {
            reloadTasks()
        }
    }

    func cancelTask(_ task: TaskItem) {
        do {
            try PiyaBackend.cancelTask(id: task.id)
            reloadTasks()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func saveTaskEdit(id: String, title: String, description: String) {
        do {
            try PiyaBackend.updateTask(id: id, title: title, description: description)
            editingTask = nil
            reloadTasks()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func resetWelcome() {
        messages = [ChatMessage(role: "piya", text: Self.welcome)]
    }

    static let welcome =
        "Hey! I'm Piya 👋\nAsk me anything — run commands, use MCP, or queue background work."

    func send() {
        let text = outgoingText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !isSending, panel == .chat else { return }

        clearComposer()
        errorMessage = nil
        messages.append(ChatMessage(role: "user", text: text))
        persistSession()
        isSending = true
        activityStatus = nil
        sendGeneration &+= 1
        let generation = sendGeneration

        let history = messages.dropLast()

        statusPoll?.cancel()
        timeoutTask?.cancel()

        statusPoll = Task { @MainActor in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 400_000_000)
                guard !Task.isCancelled else { return }
                let status = PiyaBackend.activityStatus()
                let next = status.isEmpty ? nil : status
                if activityStatus != next {
                    activityStatus = next
                }
            }
        }

        timeoutTask = Task { @MainActor in
            try? await Task.sleep(for: .seconds(Self.sendTimeoutSeconds))
            guard sendGeneration == generation, isSending else { return }
            stopTurn(timedOut: true)
        }

        Task.detached(priority: .userInitiated) {
            do {
                let reply = try PiyaBackend.send(
                    message: text,
                    history: Array(history)
                )
                await MainActor.run {
                    guard self.sendGeneration == generation else { return }
                    self.finishTurn()
                    self.messages.append(ChatMessage(role: "piya", text: reply))
                    self.persistSession()
                    self.reloadTasks()
                }
            } catch {
                await MainActor.run {
                    guard self.sendGeneration == generation else { return }
                    let message = error.localizedDescription
                    self.finishTurn()
                    if message.localizedCaseInsensitiveContains("cancelled") {
                        return
                    }
                    self.errorMessage = message
                    self.messages.append(
                        ChatMessage(
                            role: "piya",
                            text: "Sorry, I couldn't respond: \(message)"
                        )
                    )
                    self.persistSession()
                }
            }
        }
    }

    func stop() {
        guard isSending else { return }
        stopTurn(timedOut: false)
    }

    private func stopTurn(timedOut: Bool) {
        PiyaBackend.cancelChat()
        sendGeneration &+= 1
        finishTurn()
        if timedOut {
            errorMessage = "Reply timed out"
            messages.append(
                ChatMessage(
                    role: "piya",
                    text: "Sorry, that reply took too long and was stopped so the app wouldn’t hang. Try again, or split the request."
                )
            )
            persistSession()
        }
    }

    private func finishTurn() {
        statusPoll?.cancel()
        timeoutTask?.cancel()
        statusPoll = nil
        timeoutTask = nil
        activityStatus = nil
        isSending = false
    }
}

struct ChatSessionItem: Identifiable, Equatable {
    let id: String
    let title: String
    let updatedAt: Date

    init(_ meta: FfiChatSessionMeta) {
        id = meta.id
        title = meta.title
        updatedAt = Date(timeIntervalSince1970: TimeInterval(meta.updatedAt))
    }
}

import AppKit
import Combine
import Foundation
import PiyaBindings
import UniformTypeIdentifiers

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
    @Published var pendingAttachments: [AttachmentItem] = []
    @Published var isStagingAttachment = false
    @Published var isRecordingVoice = false
    @Published var isSending = false
    @Published var activityStatus: String?
    @Published var errorMessage: String?
    @Published var editingTask: TaskItem?
    /// Bumped when the active session's messages are replaced so the list can settle scroll.
    @Published private(set) var sessionContentEpoch: UInt64 = 0

    let speech = SpeechDictationController()
    private var speechCancellables: Set<AnyCancellable> = []
    private var dictationBaseInput = ""
    private var holdToTalkActive = false

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

    var hasOutgoingContent: Bool {
        hasOutgoingText || !pendingAttachments.isEmpty
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
        pendingAttachments = []
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
        speech.$isRecording
            .receive(on: RunLoop.main)
            .sink { [weak self] rec in
                self?.isRecordingVoice = rec
            }
            .store(in: &speechCancellables)

        speech.$errorMessage
            .compactMap { $0 }
            .receive(on: RunLoop.main)
            .sink { [weak self] err in
                self?.errorMessage = err
            }
            .store(in: &speechCancellables)

        reloadSessions(selectActive: true)
        reloadTasks()
    }

    // MARK: - Voice dictation & push-to-talk

    func toggleVoiceDictation() {
        if speech.isRecording {
            speech.stop()
            holdToTalkActive = false
        } else {
            beginDictation()
        }
    }

    func startHoldToTalk() {
        guard !holdToTalkActive, !speech.isRecording, panel == .chat else { return }
        holdToTalkActive = true
        beginDictation()
    }

    func stopHoldToTalk() {
        guard holdToTalkActive else { return }
        holdToTalkActive = false
        speech.stop()
    }

    private func beginDictation() {
        let existing = input.trimmingCharacters(in: .whitespacesAndNewlines)
        dictationBaseInput = existing.isEmpty ? "" : (existing + " ")
        speech.start { [weak self] transcript in
            guard let self else { return }
            self.input = self.dictationBaseInput + transcript
            self.absorbLargeInputIfNeeded()
        }
    }

    // MARK: - Media & file attachments

    func openAttachmentPicker() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.prompt = "Attach"
        panel.message = "Attach images, PDFs, documents, CSVs, or audio files"
        NSApp.activate(ignoringOtherApps: true)
        if panel.runModal() == .OK, !panel.urls.isEmpty {
            attachFileURLs(panel.urls)
        }
    }

    func removePendingAttachment(_ id: String) {
        pendingAttachments.removeAll { $0.id == id }
    }

    /// Returns `true` when the clipboard held files or an image (screenshot) and was staged.
    func pasteAttachmentFromClipboard() -> Bool {
        let pb = NSPasteboard.general
        if let urls = pb.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL], !urls.isEmpty {
            attachFileURLs(urls)
            return true
        }

        if let pngData = Self.clipboardPNGData(from: pb) {
            let stamp = Int(Date().timeIntervalSince1970)
            let tempURL = URL(fileURLWithPath: NSTemporaryDirectory())
                .appendingPathComponent("Screenshot-\(stamp).png")
            do {
                try pngData.write(to: tempURL, options: .atomic)
                attachFileURLs([tempURL], displayNameOverride: "Screenshot.png")
                return true
            } catch {
                errorMessage = "Could not paste image: \(error.localizedDescription)"
                return true
            }
        }
        return false
    }

    private static func clipboardPNGData(from pb: NSPasteboard) -> Data? {
        if let data = pb.data(forType: .png), !data.isEmpty {
            return data
        }
        if let tiff = pb.data(forType: .tiff),
           let rep = NSBitmapImageRep(data: tiff),
           let png = rep.representation(using: .png, properties: [:])
        {
            return png
        }
        return nil
    }

    func attachFileURLs(_ urls: [URL], displayNameOverride: String? = nil) {
        guard !urls.isEmpty else { return }
        let sessionId = activeSessionId.isEmpty ? "active" : activeSessionId
        isStagingAttachment = true
        errorMessage = nil

        Task.detached(priority: .userInitiated) {
            var staged: [AttachmentItem] = []
            var firstError: String?
            for url in urls {
                let extracted = await MediaExtractor.extractText(from: url)
                do {
                    let item = try PiyaBackend.stageAttachment(
                        sessionId: sessionId,
                        sourcePath: url.path,
                        displayName: displayNameOverride ?? url.lastPathComponent,
                        extractedText: extracted
                    )
                    staged.append(item)
                } catch {
                    if firstError == nil {
                        firstError = error.localizedDescription
                    }
                }
            }
            let finalStaged = staged
            let finalError = firstError
            await MainActor.run {
                self.isStagingAttachment = false
                if !finalStaged.isEmpty {
                    self.pendingAttachments.append(contentsOf: finalStaged)
                }
                if let finalError {
                    self.errorMessage = finalError
                }
            }
        }
    }

    // MARK: - Sessions & tasks

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
        if speech.isRecording { speech.stop() }
        persistSessionIfNeeded()
        do {
            try PiyaBackend.selectSession(id: id)
            activeSessionId = id
            pendingAttachments = []
            loadActiveSessionContent(markEpoch: true)
            refreshSessionList()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func newSession() {
        guard !isSending else { return }
        if speech.isRecording { speech.stop() }
        persistSessionIfNeeded()
        do {
            let meta = try PiyaBackend.createSession()
            activeSessionId = meta.id
            pendingAttachments = []
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
        "Hey! I'm Piya 👋\nAsk me anything — talk by voice, drop images/PDFs/audio, run commands, or queue work."

    func send() {
        if speech.isRecording {
            speech.stop()
            holdToTalkActive = false
        }
        let text = outgoingText.trimmingCharacters(in: .whitespacesAndNewlines)
        let attachments = pendingAttachments
        guard (!text.isEmpty || !attachments.isEmpty), !isSending, !isStagingAttachment, panel == .chat else {
            return
        }

        clearComposer()
        errorMessage = nil
        messages.append(ChatMessage(role: "user", text: text, attachments: attachments))
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
                    attachments: attachments,
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

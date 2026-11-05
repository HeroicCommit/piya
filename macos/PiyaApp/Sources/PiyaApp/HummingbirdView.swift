import AppKit
import SwiftUI

struct HummingbirdView: View {
    @ObservedObject var model: ChatViewModel

    var body: some View {
        HStack(spacing: 0) {
            if model.panel == .chat && model.showSidebar {
                sessionSidebar
                Divider()
            }
            VStack(spacing: 0) {
                header
                Divider()
                Group {
                    if model.panel == .chat {
                        chatPanel
                    } else {
                        TasksView(model: model)
                    }
                }
                .frame(maxHeight: .infinity)
            }
        }
    }

    private var sessionSidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Chats")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Spacer()
                Button(action: { model.newSession() }) {
                    Image(systemName: "square.and.pencil")
                        .font(.caption.weight(.semibold))
                }
                .buttonStyle(.plain)
                .help("New chat")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)

            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(model.sessions) { session in
                        sessionRow(session)
                    }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 8)
            }
        }
        .frame(width: 180)
        .background(Color.primary.opacity(0.03))
    }

    private func sessionRow(_ session: ChatSessionItem) -> some View {
        let selected = session.id == model.activeSessionId
        return Button {
            model.selectSession(session.id)
        } label: {
            VStack(alignment: .leading, spacing: 3) {
                Text(session.title)
                    .font(.caption.weight(selected ? .semibold : .regular))
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
                    .foregroundStyle(selected ? .primary : .secondary)
                TimeAgoLabel(date: session.updatedAt)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 8)
            .padding(.vertical, 7)
            .background(
                selected ? Color.accentColor.opacity(0.14) : Color.clear,
                in: RoundedRectangle(cornerRadius: 8, style: .continuous)
            )
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button("Delete", role: .destructive) {
                model.deleteSession(session.id)
            }
            .disabled(model.sessions.count <= 1)
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            if model.panel == .chat {
                Button {
                    model.showSidebar.toggle()
                } label: {
                    Image(systemName: "sidebar.left")
                        .font(.body.weight(.medium))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help(model.showSidebar ? "Hide chats" : "Show chats")
            }

            Circle()
                .fill(
                    LinearGradient(
                        colors: [
                            Color(red: 0.98, green: 0.66, blue: 0.83),
                            Color(red: 0.93, green: 0.28, blue: 0.6),
                        ],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    )
                )
                .frame(width: 32, height: 32)
            VStack(alignment: .leading, spacing: 2) {
                Text("Piya")
                    .font(.headline)
                Text(model.panel.subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if model.panel == .chat {
                Button(action: { model.newSession() }) {
                    Image(systemName: "plus")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("New chat")
            } else {
                Button(action: { model.reloadTasks() }) {
                    Image(systemName: "arrow.clockwise")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Refresh tasks")
            }
            panelSwitch
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    private var panelSwitch: some View {
        HStack(spacing: 2) {
            ForEach(AppPanel.allCases, id: \.self) { panel in
                Button {
                    model.setPanel(panel)
                } label: {
                    Text(panel.label)
                        .font(.caption.weight(.semibold))
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                        .background(
                            model.panel == panel ? Color.primary.opacity(0.08) : Color.clear,
                            in: Capsule()
                        )
                        .foregroundStyle(model.panel == panel ? .primary : .secondary)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(3)
        .background(.quaternary.opacity(0.5), in: Capsule())
    }

    private var chatPanel: some View {
        VStack(spacing: 0) {
            messageList
            Divider()
            composer
            Text("Enter to send · ⌘↩ or Shift+↩ for new line")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .padding(.bottom, 8)
        }
    }

    private var messageList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(model.messages) { msg in
                        MessageRow(msg: msg)
                            .id(msg.id)
                    }
                    if model.isSending {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text(model.activityStatus ?? "Thinking…")
                                .font(.caption.monospaced())
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                        .padding(.leading, 4)
                    }
                    Color.clear
                        .frame(height: 1)
                        .id(Self.scrollBottomID)
                }
                .padding(16)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .onChange(of: model.messages.count) { old, new in
                guard new > old else { return }
                jumpToBottom(proxy)
            }
            .onChange(of: model.sessionContentEpoch) { _, _ in
                jumpToBottom(proxy)
            }
            .onChange(of: model.isSending) { _, sending in
                guard sending else { return }
                jumpToBottom(proxy)
            }
        }
    }

    private static let scrollBottomID = "chat-scroll-bottom"

    /// One-shot scroll — never bind `scrollPosition`, which feedback-loops AttributeGraph.
    private func jumpToBottom(_ proxy: ScrollViewProxy) {
        DispatchQueue.main.async {
            proxy.scrollTo(Self.scrollBottomID, anchor: .bottom)
        }
    }

    private var composer: some View {
        HStack(alignment: .bottom, spacing: 8) {
            if model.largePaste != nil {
                largePasteComposer
            } else {
                ComposerTextView(
                    text: $model.input,
                    onSubmit: {
                        if model.hasOutgoingText, !model.isSending {
                            model.send()
                        }
                    },
                    onLargePaste: { model.attachPasteFromClipboard() }
                )
                .frame(height: 36)
                .onChange(of: model.input) { _, _ in
                    model.absorbLargeInputIfNeeded()
                }
            }
            Button(action: {
                if model.isSending {
                    model.stop()
                } else {
                    model.send()
                }
            }) {
                Image(systemName: model.isSending ? "stop.circle.fill" : "arrow.up.circle.fill")
                    .font(.title2)
                    .foregroundStyle(model.isSending ? Color.primary.opacity(0.85) : Color.accentColor)
            }
            .buttonStyle(.plain)
            .disabled(!model.isSending && !model.hasOutgoingText)
            .help(model.isSending ? "Stop" : "Send")
        }
        .padding(12)
    }

    private var largePasteComposer: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: "doc.on.clipboard")
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text("Large paste ready")
                    .font(.caption.weight(.semibold))
                Text(MessageMarkdown.sizeLabel(model.largePaste ?? ""))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            Button("Clear") {
                model.clearComposer()
            }
            .font(.caption.weight(.semibold))
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.06), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .onKeyPress(.return, phases: .down) { _ in
            if !model.isSending {
                model.send()
            }
            return .handled
        }
    }
}

/// Hover/copy state stays here so the rest of Hummingbird is not invalidated.
private struct MessageRow: View {
    let msg: ChatMessage
    @State private var hovering = false
    @State private var copied = false

    var body: some View {
        let isUser = msg.role == "user"
        HStack(alignment: .top, spacing: 6) {
            if isUser { Spacer(minLength: 48) }
            MessageBubbleContent(text: msg.text, isUser: isUser)
                .overlay(alignment: isUser ? .topLeading : .topTrailing) {
                    if hovering || copied {
                        copyButton
                            .offset(x: isUser ? -4 : 4, y: -4)
                    }
                }
                .padding(4)
                .contentShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                .onHover { hovering = $0 }
            if !isUser { Spacer(minLength: 48) }
        }
        .frame(maxWidth: .infinity, alignment: isUser ? .trailing : .leading)
    }

    private var copyButton: some View {
        Button {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(msg.text, forType: .string)
            copied = true
            Task {
                try? await Task.sleep(for: .seconds(1.5))
                copied = false
            }
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.secondary)
                .padding(5)
                .background(Color.primary.opacity(0.08), in: Circle())
        }
        .buttonStyle(.plain)
        .help(copied ? "Copied" : "Copy message")
    }
}

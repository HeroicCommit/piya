import AppKit
import SwiftUI

/// One segment of a chat message: prose or a fenced code block.
enum MessageSegment: Identifiable, Equatable {
    case text(String)
    case code(language: String?, code: String)

    var id: String {
        switch self {
        case .text(let s):
            // Prefer length + prefix — hashing full huge strings is expensive.
            let n = s.utf16.count
            let head = s.prefix(48)
            return "t:\(n):\(head)"
        case .code(let lang, let code):
            let n = code.utf16.count
            return "c:\(lang ?? ""):\(n):\(code.prefix(32))"
        }
    }
}

enum MessageMarkdown {
    /// Above this, skip markdown + linkify.
    static let richParseLimit = 4_000
    /// Above this, collapse the bubble until the user expands it.
    static let collapseLimit = 2_000
    /// Composer switches to a compact paste chip above this size.
    static let composerCompactLimit = 1_200

    /// O(1) length for ASCII-heavy dumps (avoids walking Character view).
    static func length(_ text: String) -> Int { text.utf16.count }

    static func isLarge(_ text: String) -> Bool { length(text) > collapseLimit }

    /// Split markdown into prose and ``` fenced ``` code blocks.
    static func segments(from text: String) -> [MessageSegment] {
        if length(text) > richParseLimit * 2 {
            return [.text(text)]
        }

        var segments: [MessageSegment] = []
        var remaining = text[...]

        while let fenceStart = remaining.range(of: "```") {
            let before = String(remaining[..<fenceStart.lowerBound])
            if !before.isEmpty {
                segments.append(.text(before))
            }

            remaining = remaining[fenceStart.upperBound...]
            let language: String?
            let bodyStart: String.SubSequence

            if let newline = remaining.firstIndex(of: "\n") {
                let langLine = remaining[..<newline].trimmingCharacters(in: .whitespacesAndNewlines)
                language = langLine.isEmpty ? nil : String(langLine)
                bodyStart = remaining[remaining.index(after: newline)...]
            } else {
                language = nil
                bodyStart = remaining
                segments.append(.code(language: language, code: String(bodyStart)))
                return segments
            }

            if let fenceEnd = bodyStart.range(of: "```") {
                var code = String(bodyStart[..<fenceEnd.lowerBound])
                if code.hasSuffix("\n") {
                    code.removeLast()
                }
                segments.append(.code(language: language, code: code))
                remaining = bodyStart[fenceEnd.upperBound...]
                if remaining.first == "\n" {
                    remaining = remaining.dropFirst()
                }
            } else {
                segments.append(.code(language: language, code: String(bodyStart)))
                return segments
            }
        }

        let rest = String(remaining)
        if !rest.isEmpty {
            segments.append(.text(rest))
        }
        if segments.isEmpty {
            segments.append(.text(text))
        }
        return segments
    }

    static func attributedProse(_ text: String) -> AttributedString {
        if length(text) > richParseLimit {
            return AttributedString(text)
        }

        var result: AttributedString
        if let markdown = try? AttributedString(
            markdown: text,
            options: AttributedString.MarkdownParsingOptions(
                interpretedSyntax: .inlineOnlyPreservingWhitespace
            )
        ) {
            result = markdown
        } else {
            result = parseBoldMarkdown(text)
        }
        linkifyBareURLs(in: &result)
        return result
    }

    private static func parseBoldMarkdown(_ text: String) -> AttributedString {
        var result = AttributedString()
        var remaining = text[...]
        var safety = 0
        while let start = remaining.range(of: "**"), safety < 200 {
            safety += 1
            result.append(AttributedString(remaining[..<start.lowerBound]))
            remaining = remaining[start.upperBound...]
            if let end = remaining.range(of: "**") {
                var bold = AttributedString(remaining[..<end.lowerBound])
                bold.inlinePresentationIntent = .stronglyEmphasized
                result.append(bold)
                remaining = remaining[end.upperBound...]
            } else {
                result.append(AttributedString("**"))
                result.append(AttributedString(remaining))
                return result
            }
        }
        result.append(AttributedString(remaining))
        return result
    }

    private static func linkifyBareURLs(in text: inout AttributedString) {
        let plain = String(text.characters)
        if plain.utf16.count > richParseLimit { return }
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else {
            return
        }
        let nsRange = NSRange(plain.startIndex..<plain.endIndex, in: plain)
        var links = 0
        detector.enumerateMatches(in: plain, options: [], range: nsRange) { match, _, stop in
            guard links < 40 else {
                stop.pointee = true
                return
            }
            guard let match, let url = match.url,
                  let range = Range(match.range, in: plain),
                  let attrRange = Range(range, in: text) else { return }
            if text[attrRange].link != nil { return }
            text[attrRange].link = url
            text[attrRange].foregroundColor = NSColor.linkColor
            text[attrRange].underlineStyle = .single
            links += 1
        }
    }

    /// Build a short preview without `split`ting the entire dump (that alone can peg CPU).
    static func preview(_ text: String, maxChars: Int = 800, maxLines: Int = 16) -> String {
        var out = String()
        out.reserveCapacity(min(maxChars + 4, 1_024))
        var lines = 0
        var chars = 0
        for ch in text {
            if ch == "\n" {
                lines += 1
                if lines >= maxLines { break }
            }
            out.append(ch)
            chars += 1
            if chars >= maxChars { break }
        }
        if chars < length(text) {
            out.append("…")
        }
        return out
    }

    static func sizeLabel(_ text: String) -> String {
        let n = length(text)
        if n >= 1_000 {
            return String(format: "%.1fK chars", Double(n) / 1_000.0)
        }
        return "\(n) chars"
    }
}

struct MessageBubbleContent: View {
    let text: String
    let isUser: Bool
    @State private var expanded = false

    private var large: Bool { MessageMarkdown.isLarge(text) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if large && !expanded {
                largeCollapsedBody
            } else if large && expanded {
                largeExpandedBody
            } else {
                smallRichBody
            }
        }
        .padding(.horizontal, large ? 10 : 12)
        .padding(.vertical, large ? 10 : 9)
        .background(isUser ? Color.accentColor.opacity(0.18) : Color.primary.opacity(0.06))
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }

    private var largeCollapsedBody: some View {
        // Capture preview once — never touch full-string markdown paths.
        let snippet = MessageMarkdown.preview(text)
        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "doc.plaintext")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("Large paste · \(MessageMarkdown.sizeLabel(text))")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
            }
            Text(snippet)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.primary.opacity(0.85))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .lineLimit(16)
            Button("Show all") { expanded = true }
                .font(.caption.weight(.semibold))
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
        }
    }

    private var largeExpandedBody: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(MessageMarkdown.sizeLabel(text))
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Show less") { expanded = false }
                    .font(.caption.weight(.semibold))
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
            }
            LargePlainTextView(text: text)
                .frame(minHeight: 180, maxHeight: 360)
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        }
    }

    private var smallRichBody: some View {
        let segments = MessageMarkdown.segments(from: text)
        return VStack(alignment: .leading, spacing: 8) {
            ForEach(segments) { segment in
                switch segment {
                case .text(let prose):
                    if !prose.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        Text(MessageMarkdown.attributedProse(prose))
                            .font(.body)
                            .tint(.accentColor)
                            .environment(\.openURL, OpenURLAction { url in
                                NSWorkspace.shared.open(url)
                                return .handled
                            })
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                case .code(let language, let code):
                    CodeBlockView(language: language, code: code)
                }
            }
        }
    }
}

struct CodeBlockView: View {
    let language: String?
    let code: String
    @State private var copied = false
    @State private var expanded = false

    private var isHuge: Bool { MessageMarkdown.isLarge(code) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text(languageLabel)
                    .font(.caption2.weight(.semibold).monospaced())
                    .foregroundStyle(.secondary)
                    .textCase(.lowercase)
                Spacer(minLength: 8)
                if isHuge {
                    Button(expanded ? "Less" : "More") { expanded.toggle() }
                        .font(.caption2.weight(.semibold))
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                }
                Button(action: copyCode) {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help(copied ? "Copied" : "Copy code")
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(Color.primary.opacity(0.06))

            if isHuge && expanded {
                LargePlainTextView(text: code)
                    .frame(minHeight: 120, maxHeight: 320)
            } else {
                Text(isHuge ? MessageMarkdown.preview(code, maxChars: 1_000, maxLines: 20) : code)
                    .font(.system(.callout, design: .monospaced))
                    .foregroundStyle(.primary)
                    .textSelection(.enabled)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .lineLimit(isHuge ? 20 : nil)
            }
        }
        .background(Color.primary.opacity(0.04))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(Color.primary.opacity(0.08), lineWidth: 1)
        )
    }

    private var languageLabel: String {
        if let language, !language.isEmpty { return language }
        return "code"
    }

    private func copyCode() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(code, forType: .string)
        copied = true
        Task {
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
    }
}

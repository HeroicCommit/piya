import AppKit
import SwiftUI

/// AppKit field — SwiftUI `TextField(axis: .vertical)` hangs Hummingbird;
/// a zero-height `NSTextView` could not take clicks.
struct ComposerTextView: NSViewRepresentable {
    @Binding var text: String
    var onSubmit: () -> Void
    var onLargePaste: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(text: $text, onSubmit: onSubmit, onLargePaste: onLargePaste)
    }

    func makeNSView(context: Context) -> ComposerNSTextField {
        let field = ComposerNSTextField()
        field.delegate = context.coordinator
        field.placeholderString = "Ask Piya anything…"
        field.isBordered = false
        field.isBezeled = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.font = NSFont.preferredFont(forTextStyle: .body)
        field.textColor = .labelColor
        field.stringValue = text
        field.usesSingleLineMode = true
        field.cell?.isScrollable = true
        field.onLargePaste = { [coord = context.coordinator] in
            coord.onLargePaste()
        }
        field.target = context.coordinator
        field.action = #selector(Coordinator.submit(_:))
        DispatchQueue.main.async {
            field.window?.makeFirstResponder(field)
        }
        return field
    }

    func updateNSView(_ field: ComposerNSTextField, context: Context) {
        context.coordinator.onSubmit = onSubmit
        context.coordinator.onLargePaste = onLargePaste
        field.onLargePaste = { [coord = context.coordinator] in
            coord.onLargePaste()
        }
        if field.stringValue != text, field.currentEditor() == nil || text.isEmpty {
            field.stringValue = text
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: ComposerNSTextField, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 240, height: 28)
    }

    final class Coordinator: NSObject, NSTextFieldDelegate {
        var text: Binding<String>
        var onSubmit: () -> Void
        var onLargePaste: () -> Void

        init(text: Binding<String>, onSubmit: @escaping () -> Void, onLargePaste: @escaping () -> Void) {
            self.text = text
            self.onSubmit = onSubmit
            self.onLargePaste = onLargePaste
        }

        func controlTextDidChange(_ obj: Notification) {
            guard let field = obj.object as? NSTextField else { return }
            text.wrappedValue = field.stringValue
        }

        @objc func submit(_ sender: Any?) {
            onSubmit()
        }
    }
}

final class ComposerNSTextField: NSTextField {
    var onLargePaste: (() -> Void)?

    override var acceptsFirstResponder: Bool { true }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        super.mouseDown(with: event)
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if event.modifierFlags.contains(.command), event.charactersIgnoringModifiers == "v" {
            if let clip = NSPasteboard.general.string(forType: .string),
               clip.count > MessageMarkdown.composerCompactLimit {
                onLargePaste?()
                return true
            }
        }
        return super.performKeyEquivalent(with: event)
    }
}

/// Read-only AppKit text view for large expanded messages (SwiftUI Text thrash).
struct LargePlainTextView: NSViewRepresentable {
    let text: String

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.drawsBackground = false

        let tv = NSTextView()
        tv.isEditable = false
        tv.isSelectable = true
        tv.isRichText = false
        tv.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        tv.textColor = .labelColor
        tv.backgroundColor = .clear
        tv.drawsBackground = false
        tv.string = text
        tv.isVerticallyResizable = true
        tv.isHorizontallyResizable = true
        tv.textContainer?.containerSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        tv.textContainer?.widthTracksTextView = false

        scroll.documentView = tv
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        (scroll.documentView as? NSTextView)?.string = text
    }
}

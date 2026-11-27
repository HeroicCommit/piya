import SwiftUI
import AppKit
import UniformTypeIdentifiers
import PiyaBindings

private enum MemorySubtab: String, CaseIterable, Identifiable {
    case notes = "Notes"
    case skills = "Skills"
    case archive = "Archive"

    var id: String { rawValue }
}

private struct NoteAreaMeta: Identifiable {
    let id: String
    let title: String
    let subtitle: String
    let icon: String

    static let all: [NoteAreaMeta] = [
        NoteAreaMeta(id: "about", title: "About you", subtitle: "Identity, projects, stack, machines", icon: "person.text.rectangle"),
        NoteAreaMeta(id: "preferences", title: "Preferences", subtitle: "Tone, formatting, tools you prefer", icon: "slider.horizontal.3"),
        NoteAreaMeta(id: "people", title: "People", subtitle: "Teammates, clients, who owns what", icon: "person.2"),
        NoteAreaMeta(id: "routines", title: "Routines", subtitle: "Recurring workflows and habits", icon: "clock.arrow.2.circlepath"),
        NoteAreaMeta(id: "lessons", title: "Lessons", subtitle: "Mistakes to avoid and corrections", icon: "checkmark.shield"),
        NoteAreaMeta(id: "opinions", title: "Piya's opinions", subtitle: "Reasoned views formed in discussions", icon: "lightbulb"),
    ]
}

private struct NoteDraft: Identifiable {
    let id = UUID()
    var noteId: String
    var area: String
    var slug: String
    var text: String
}

private struct SkillDraft: Identifiable {
    let id = UUID()
    var name: String
    var description: String
    var body: String
    var isNew: Bool
}

struct MemorySettingsTab: View {
    @State private var subtab: MemorySubtab = .notes
    @State private var notes: [FfiNote] = []
    @State private var skills: [FfiLearnedSkill] = []
    @State private var archive: [FfiMemoryEntry] = []

    @State private var learnFromConversations = true
    @State private var rememberSensitive = true
    @State private var reflecting = false
    @State private var filter = ""
    @State private var status = ""

    @State private var editingNote: NoteDraft?
    @State private var editingSkill: SkillDraft?

    var body: some View {
        VStack(spacing: 0) {
            topControls
            Divider()
            Form {
                switch subtab {
                case .notes:
                    notesContent
                case .skills:
                    skillsContent
                case .archive:
                    archiveContent
                }
            }
            .formStyle(.grouped)
        }
        .onAppear(perform: reloadAll)
        .sheet(item: $editingNote) { draft in
            NoteEditorSheet(draft: draft) { saved in
                upsertNote(saved)
                status = "Saved note \(saved.area)/\(saved.slug)."
            }
        }
        .sheet(item: $editingSkill) { draft in
            SkillEditorSheet(draft: draft) { saved in
                upsertSkill(saved)
                status = "Saved skill \(saved.name)."
            }
        }
    }

    // MARK: - Header controls

    private var topControls: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                Picker("", selection: $subtab) {
                    ForEach(MemorySubtab.allCases) { tab in
                        Text(subtabLabel(tab)).tag(tab)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(maxWidth: 320)

                TextField("Filter…", text: $filter)
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 220)

                Spacer()

                if subtab == .notes {
                    Button {
                        editingNote = NoteDraft(noteId: "", area: "about", slug: "", text: "")
                    } label: {
                        Label("New Note", systemImage: "plus")
                    }
                } else if subtab == .skills {
                    Button {
                        editingSkill = SkillDraft(name: "", description: "", body: "", isNew: true)
                    } label: {
                        Label("New Skill", systemImage: "plus")
                    }
                }

                Button {
                    exportMarkdown()
                } label: {
                    Label("Export…", systemImage: "square.and.arrow.up")
                }
                .help("Export active notes and learned skills to a Markdown file")
            }

            HStack(spacing: 16) {
                Toggle("Daily reflection", isOn: Binding(
                    get: { learnFromConversations },
                    set: { val in
                        learnFromConversations = val
                        savePrefs()
                    }
                ))
                .controlSize(.small)

                Toggle("Sensitive topics & opinions", isOn: Binding(
                    get: { rememberSensitive },
                    set: { val in
                        rememberSensitive = val
                        savePrefs()
                    }
                ))
                .controlSize(.small)

                Button {
                    triggerReflection()
                } label: {
                    if reflecting {
                        HStack(spacing: 5) {
                            ProgressView().controlSize(.small)
                            Text("Reflecting…")
                        }
                    } else {
                        Label("Reflect Now", systemImage: "sparkles")
                    }
                }
                .controlSize(.small)
                .disabled(reflecting)

                Spacer()

                if !status.isEmpty {
                    Text(status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    private func subtabLabel(_ tab: MemorySubtab) -> String {
        switch tab {
        case .notes: return "Notes (\(notes.count))"
        case .skills: return "Skills (\(skills.count))"
        case .archive: return "Archive (\(archive.count))"
        }
    }

    // MARK: - Notes View

    private var filteredNotes: [FfiNote] {
        let q = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return notes }
        return notes.filter {
            $0.text.lowercased().contains(q)
                || $0.slug.lowercased().contains(q)
                || $0.area.lowercased().contains(q)
                || $0.quote.lowercased().contains(q)
        }
    }

    @ViewBuilder
    private var notesContent: some View {
        if filteredNotes.isEmpty {
            Section {
                VStack(alignment: .leading, spacing: 6) {
                    Text(notes.isEmpty
                         ? "No personal notes yet."
                         : "No notes match “\(filter)”.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    Text("Active notes stay in Piya's context on every chat turn so she remembers your setup, preferences, people, lessons, and opinions across conversations.")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                .padding(.vertical, 6)
            }
        } else {
            ForEach(NoteAreaMeta.all) { meta in
                let areaNotes = filteredNotes.filter { $0.area == meta.id }
                if !areaNotes.isEmpty {
                    Section {
                        ForEach(areaNotes, id: \.id) { note in
                            noteRow(note)
                        }
                    } header: {
                        HStack(spacing: 6) {
                            Image(systemName: meta.icon)
                            Text(meta.title)
                            Text("— \(meta.subtitle)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }

    private func noteRow(_ note: FfiNote) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(note.slug)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)

                statusBadge(note.status)

                if !note.expiresAt.isEmpty {
                    Text("until \(note.expiresAt)")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }

                Spacer()

                if note.status == "tentative" {
                    Button("Confirm") {
                        confirm(note)
                    }
                    .controlSize(.small)
                }

                Button("Edit") {
                    editingNote = NoteDraft(
                        noteId: note.id,
                        area: note.area,
                        slug: note.slug,
                        text: note.text
                    )
                }
                .controlSize(.small)

                Button("Forget", role: .destructive) {
                    forget(note)
                }
                .controlSize(.small)
            }

            Text(note.text)
                .font(.callout)
                .textSelection(.enabled)

            if !note.quote.isEmpty {
                Text("“\(note.quote)”")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .lineLimit(2)
            }
        }
        .padding(.vertical, 4)
    }

    private func statusBadge(_ status: String) -> some View {
        let (label, color): (String, Color) = {
            switch status {
            case "confirmed": return ("Confirmed", .green)
            case "learned": return ("Learned", .blue)
            case "tentative": return ("Tentative", .orange)
            default: return (status.capitalized, .secondary)
            }
        }()
        return Text(label)
            .font(.caption2.weight(.medium))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.14), in: Capsule())
            .foregroundStyle(color)
    }

    // MARK: - Skills View

    private var filteredSkills: [FfiLearnedSkill] {
        let q = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return skills }
        return skills.filter {
            $0.name.lowercased().contains(q)
                || $0.description.lowercased().contains(q)
                || $0.body.lowercased().contains(q)
        }
    }

    @ViewBuilder
    private var skillsContent: some View {
        Section {
            if filteredSkills.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text(skills.isEmpty
                         ? "No learned skills yet."
                         : "No skills match “\(filter)”.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    Text("When Piya solves a multi-step workflow or workaround, she saves a reusable skill here so she doesn't repeat trial-and-error next time.")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                .padding(.vertical, 6)
            } else {
                ForEach(filteredSkills, id: \.name) { skill in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(spacing: 8) {
                            Text(skill.name)
                                .font(.system(.callout, design: .monospaced).weight(.semibold))
                            if skill.userEdited {
                                Label("Locked by you", systemImage: "lock.fill")
                                    .font(.caption2)
                                    .foregroundStyle(.blue)
                            }
                            if skill.useCount > 0 {
                                Text("used \(skill.useCount)×")
                                    .font(.caption2)
                                    .foregroundStyle(.tertiary)
                            }
                            Spacer()
                            Button("Edit") {
                                editingSkill = SkillDraft(
                                    name: skill.name,
                                    description: skill.description,
                                    body: skill.body,
                                    isNew: false
                                )
                            }
                            .controlSize(.small)
                            Button("Delete", role: .destructive) {
                                removeSkill(skill.name)
                            }
                            .controlSize(.small)
                        }
                        Text(skill.description)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 4)
                }
            }
        } header: {
            Text("Learned procedural skills")
        } footer: {
            Text("Skills you edit here are locked so Piya asks before changing them.")
                .font(.caption)
        }
    }

    // MARK: - Archive View

    private var filteredArchive: [FfiMemoryEntry] {
        let q = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return archive }
        return archive.filter {
            $0.text.lowercased().contains(q) || $0.kind.lowercased().contains(q)
        }
    }

    @ViewBuilder
    private var archiveContent: some View {
        Section {
            if filteredArchive.isEmpty {
                Text(archive.isEmpty ? "No background task memories archived." : "No matches.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(filteredArchive, id: \.id) { item in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(item.kind)
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(.quaternary, in: Capsule())
                            if !item.source.isEmpty {
                                Text(item.source)
                                    .font(.caption2)
                                    .foregroundStyle(.tertiary)
                            }
                            Spacer()
                            Button("Forget", role: .destructive) {
                                deleteArchiveItem(item.id)
                            }
                            .controlSize(.small)
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
            Text("Background task & vector archive")
        } footer: {
            Text("Raw research snippets and task outcomes searched on demand.")
                .font(.caption)
        }
    }

    // MARK: - Actions

    private func reloadAll() {
        if let prefs = try? PiyaBackend.userPrefs() {
            learnFromConversations = prefs.learnFromConversations
            rememberSensitive = prefs.rememberSensitive
        }
        notes = (try? PiyaBackend.listNotes(includeArchived: false)) ?? []
        skills = (try? PiyaBackend.listSkills()) ?? []
        archive = (try? PiyaBackend.listMemories(limit: 200)) ?? []
    }

    private func savePrefs() {
        do {
            try PiyaBackend.saveLearningPrefs(
                learnFromConversations: learnFromConversations,
                rememberSensitive: rememberSensitive
            )
            status = "Preferences saved."
        } catch {
            status = error.localizedDescription
        }
    }

    private func confirm(_ note: FfiNote) {
        do {
            let updated = try PiyaBackend.confirmNote(id: note.id)
            upsertNote(updated)
            status = "Confirmed \(updated.area)/\(updated.slug)."
        } catch {
            status = error.localizedDescription
        }
    }

    private func forget(_ note: FfiNote) {
        do {
            try PiyaBackend.forgetNote(id: note.id)
            notes.removeAll { $0.id == note.id }
            status = "Forgot \(note.area)/\(note.slug)."
        } catch {
            status = error.localizedDescription
        }
    }

    private func upsertNote(_ saved: FfiNote) {
        if let idx = notes.firstIndex(where: { $0.id == saved.id }) {
            notes[idx] = saved
        } else {
            notes.insert(saved, at: 0)
        }
    }

    private func upsertSkill(_ saved: FfiLearnedSkill) {
        if let idx = skills.firstIndex(where: { $0.name == saved.name }) {
            skills[idx] = saved
        } else {
            skills.insert(saved, at: 0)
        }
    }

    private func removeSkill(_ name: String) {
        do {
            try PiyaBackend.deleteSkill(name: name)
            skills.removeAll { $0.name == name }
            status = "Deleted skill \(name)."
        } catch {
            status = error.localizedDescription
        }
    }

    private func deleteArchiveItem(_ id: String) {
        do {
            try PiyaBackend.deleteMemory(id: id)
            archive.removeAll { $0.id == id }
            status = "Removed from archive."
        } catch {
            status = error.localizedDescription
        }
    }

    private func triggerReflection() {
        reflecting = true
        status = "Reviewing recent conversations…"
        Task.detached {
            let result = Result { try PiyaBackend.runReflection(force: true) }
            await MainActor.run {
                reflecting = false
                switch result {
                case .success(let summary):
                    reloadAll()
                    status = summary
                case .failure(let err):
                    status = err.localizedDescription
                }
            }
        }
    }

    private func exportMarkdown() {
        do {
            let md = try PiyaBackend.exportMemoryMarkdown()
            let panel = NSSavePanel()
            panel.allowedContentTypes = [.plainText]
            panel.nameFieldStringValue = "piya-memory.md"
            if panel.runModal() == .OK, let url = panel.url {
                try md.write(to: url, atomically: true, encoding: .utf8)
                status = "Exported to \(url.lastPathComponent)."
            }
        } catch {
            status = error.localizedDescription
        }
    }
}

// MARK: - Editors

private struct NoteEditorSheet: View {
    @Environment(\.dismiss) private var dismiss
    @State var draft: NoteDraft
    let onSave: (FfiNote) -> Void
    @State private var errorText = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(draft.noteId.isEmpty ? "New Memory Note" : "Edit Memory Note")
                .font(.headline)

            Picker("Area", selection: $draft.area) {
                ForEach(NoteAreaMeta.all) { meta in
                    Text(meta.title).tag(meta.id)
                }
            }

            if draft.noteId.isEmpty {
                TextField("Optional short key (e.g. staging-ip)", text: $draft.slug)
                    .textFieldStyle(.roundedBorder)
            }

            Text("Note")
                .font(.caption)
                .foregroundStyle(.secondary)
            TextEditor(text: $draft.text)
                .font(.body)
                .frame(minHeight: 110)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))

            if !errorText.isEmpty {
                Text(errorText)
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Save") {
                    do {
                        let saved = try PiyaBackend.saveNote(
                            id: draft.noteId,
                            area: draft.area,
                            slug: draft.slug,
                            text: draft.text
                        )
                        onSave(saved)
                        dismiss()
                    } catch {
                        errorText = error.localizedDescription
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 480)
    }
}

private struct SkillEditorSheet: View {
    @Environment(\.dismiss) private var dismiss
    @State var draft: SkillDraft
    let onSave: (FfiLearnedSkill) -> Void
    @State private var errorText = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(draft.isNew ? "New Learned Skill" : "Edit Skill: \(draft.name)")
                .font(.headline)

            if draft.isNew {
                TextField("Skill name (e.g. deploy-docs)", text: $draft.name)
                    .textFieldStyle(.roundedBorder)
            }

            TextField("One-line trigger description (when Piya should use this)", text: $draft.description)
                .textFieldStyle(.roundedBorder)

            Text("Procedure (Markdown steps, commands, gotchas)")
                .font(.caption)
                .foregroundStyle(.secondary)
            TextEditor(text: $draft.body)
                .font(.system(.body, design: .monospaced))
                .frame(minHeight: 200)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))

            if !errorText.isEmpty {
                Text(errorText)
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Save & Lock") {
                    do {
                        let saved = try PiyaBackend.saveSkill(
                            name: draft.name,
                            description: draft.description,
                            body: draft.body
                        )
                        onSave(saved)
                        dismiss()
                    } catch {
                        errorText = error.localizedDescription
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(
                    draft.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || draft.description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || draft.body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )
            }
        }
        .padding(20)
        .frame(width: 560)
    }
}

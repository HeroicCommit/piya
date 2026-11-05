import SwiftUI

struct TasksView: View {
    @ObservedObject var model: ChatViewModel
    @State private var editTitle = ""
    @State private var editDescription = ""

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                if model.tasks.isEmpty {
                    emptyState
                } else {
                    ForEach(TaskCategory.allCases, id: \.self) { category in
                        let items = model.tasks.filter { $0.category == category }
                        if !items.isEmpty {
                            section(category: category, items: items)
                        }
                    }
                }
            }
            .padding(16)
        }
        .refreshable { model.reloadTasks() }
        .onAppear { model.reloadTasks() }
        .sheet(item: $model.editingTask) { task in
            editSheet(task)
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("No active tasks")
                .font(.headline)
            Text("Queue work from **Chat** — e.g. “Research X and write workspace/report.md” or “Every day at 6am send stats”.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, 24)
    }

    private func section(category: TaskCategory, items: [TaskItem]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(category.title)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)

            ForEach(items) { task in
                taskRow(task)
            }
        }
    }

    private func taskRow(_ task: TaskItem) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(task.title)
                        .font(.body.weight(.semibold))
                    Text(task.description)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(3)
                    HStack(spacing: 6) {
                        Label(task.scheduleLabel, systemImage: task.isRecurring ? "repeat" : "clock")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                        if task.category == .running {
                            Text("In progress")
                                .font(.caption2.weight(.medium))
                                .foregroundStyle(.orange)
                        }
                    }
                }
                Spacer(minLength: 8)
                HStack(spacing: 4) {
                    Button {
                        editTitle = task.title
                        editDescription = task.description
                        model.editingTask = task
                    } label: {
                        Image(systemName: "pencil")
                    }
                    .buttonStyle(.plain)
                    .help("Edit task")

                    Button(role: .destructive) {
                        model.cancelTask(task)
                    } label: {
                        Image(systemName: "xmark.circle")
                    }
                    .buttonStyle(.plain)
                    .help("Cancel task")
                }
                .foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .background(Color.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    private func editSheet(_ task: TaskItem) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Edit task")
                .font(.headline)

            TextField("Title", text: $editTitle)
                .textFieldStyle(.roundedBorder)

            Text("Description")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            TextEditor(text: $editDescription)
                .font(.body)
                .frame(minHeight: 120)
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(Color.primary.opacity(0.12))
                )

            HStack {
                Button("Cancel") {
                    model.editingTask = nil
                }
                .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Save") {
                    model.saveTaskEdit(
                        id: task.id,
                        title: editTitle,
                        description: editDescription
                    )
                }
                .keyboardShortcut(.defaultAction)
                .disabled(editDescription.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 420)
    }
}

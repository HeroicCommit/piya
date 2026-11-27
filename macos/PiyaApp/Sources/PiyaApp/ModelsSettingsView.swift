import AppKit
import PiyaBindings
import SwiftUI

/// Settings → Models: one active provider + model for everything, and a separate
/// memory (embeddings) model because many chat providers have no embeddings API.
struct ModelsSettingsTab: View {
    @State private var providers: [FfiProvider] = []

    // Chat
    @State private var provider = "openai"
    @State private var apiKey = ""
    @State private var baseUrl = ""
    @State private var model = ""
    @State private var models: [String] = []
    @State private var revealKey = false
    @State private var loadingModels = false
    @State private var chatStatus: (ok: Bool, text: String)?

    // Memory
    @State private var embedProvider = "none"
    @State private var embedApiKey = ""
    @State private var embedBaseUrl = ""
    @State private var embeddingModel = ""
    @State private var embedModels: [String] = []
    @State private var embedStatus: (ok: Bool, text: String)?

    @State private var original: FfiModelSettings?
    @State private var status = ""

    private var info: FfiProvider? { providers.first { $0.id == provider } }
    private var embedInfo: FfiProvider? { providers.first { $0.id == embedProvider } }
    private var embedProviders: [FfiProvider] { providers.filter { $0.supportsEmbeddings } }
    private var embedSharesChatKey: Bool { embedProvider == provider }
    private var keyWarning: String { PiyaBackend.keyWarning(provider: provider, apiKey: apiKey) }

    var body: some View {
        Form {
            Section("Model") {
                Picker("Provider", selection: $provider) {
                    ForEach(providers, id: \.id) { p in
                        Text(p.label).tag(p.id)
                    }
                }
                .onChange(of: provider) { _, new in switchChatProvider(to: new) }

                if let info {
                    if info.needsKey || info.id == "custom" {
                        keyField(info.needsKey ? "API key" : "API key (optional)", text: $apiKey)
                        if !keyWarning.isEmpty {
                            Label(keyWarning, systemImage: "exclamationmark.triangle.fill")
                                .font(.caption)
                                .foregroundStyle(.orange)
                        }
                    }
                    if info.local || info.id == "custom" {
                        TextField("Server address", text: $baseUrl, prompt: Text(info.defaultBaseUrl.isEmpty ? "http://host:port/v1" : info.defaultBaseUrl))
                    } else {
                        DisclosureGroup("Server address") {
                            TextField("Base URL", text: $baseUrl, prompt: Text(info.defaultBaseUrl))
                        }
                        .font(.callout)
                    }
                    if !info.keyUrl.isEmpty, let url = URL(string: info.keyUrl) {
                        Link(info.local ? "Download \(info.label)…" : "Get a \(info.label) key…", destination: url)
                            .font(.caption)
                    }
                }

                modelPicker(title: "Model", selection: $model, options: models, placeholder: "model id")
                HStack {
                    Button(loadingModels ? "Loading…" : "Refresh models") { refreshModels() }
                        .disabled(loadingModels)
                    Button("Test") { testChat() }
                        .disabled(model.trimmingCharacters(in: .whitespaces).isEmpty)
                    Spacer()
                }
                statusLine(chatStatus)
            }

            Section("Memory (embeddings)") {
                Picker("Provider", selection: $embedProvider) {
                    Text("Off — no memory search").tag("none")
                    ForEach(embedProviders, id: \.id) { p in
                        Text(p.label).tag(p.id)
                    }
                }
                .onChange(of: embedProvider) { _, new in switchEmbedProvider(to: new) }

                if let e = embedInfo {
                    if embedSharesChatKey {
                        Text("Uses the same \(e.label) connection as the chat model.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else {
                        if e.needsKey || e.id == "custom" {
                            keyField("API key", text: $embedApiKey)
                        }
                        if e.local || e.id == "custom" {
                            TextField("Server address", text: $embedBaseUrl, prompt: Text(e.defaultBaseUrl))
                        }
                    }
                    modelPicker(title: "Embeddings model", selection: $embeddingModel, options: embedModels, placeholder: e.defaultEmbeddingModel)
                    HStack {
                        Button("Refresh models") { refreshEmbedModels() }
                        Button("Test") { testEmbeddings() }
                            .disabled(embeddingModel.trimmingCharacters(in: .whitespaces).isEmpty)
                        Spacer()
                    }
                    statusLine(embedStatus)
                }
                Text("Memory search turns notes into vectors with this model. Changing it re-indexes saved memories automatically (a backup is kept in the data folder).")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if !status.isEmpty {
                Section { Text(status).font(.caption).foregroundStyle(.secondary) }
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

    // MARK: Pieces

    @ViewBuilder
    private func keyField(_ label: String, text: Binding<String>) -> some View {
        LabeledContent(label) {
            HStack(spacing: 6) {
                Group {
                    if revealKey {
                        TextField("", text: text)
                    } else {
                        SecureField("", text: text)
                    }
                }
                .textFieldStyle(.roundedBorder)
                .frame(width: 240)
                Button { revealKey.toggle() } label: {
                    Image(systemName: revealKey ? "eye.slash" : "eye")
                }
                .buttonStyle(.borderless)
                .help(revealKey ? "Hide key" : "Reveal key")
            }
        }
    }

    @ViewBuilder
    private func modelPicker(title: String, selection: Binding<String>, options: [String], placeholder: String) -> some View {
        if options.isEmpty {
            TextField(title, text: selection, prompt: Text(placeholder.isEmpty ? "model id" : placeholder))
        } else {
            Picker(title, selection: selection) {
                if !options.contains(selection.wrappedValue) {
                    Text(selection.wrappedValue.isEmpty ? "Choose…" : selection.wrappedValue).tag(selection.wrappedValue)
                }
                ForEach(options, id: \.self) { Text($0).tag($0) }
            }
            TextField("Or type a model id", text: selection)
                .font(.caption)
        }
    }

    @ViewBuilder
    private func statusLine(_ s: (ok: Bool, text: String)?) -> some View {
        if let s {
            Label(s.text, systemImage: s.ok ? "checkmark.circle.fill" : "xmark.octagon.fill")
                .font(.caption)
                .foregroundStyle(s.ok ? .green : .red)
                .textSelection(.enabled)
        }
    }

    // MARK: Actions

    private func load() {
        providers = PiyaBackend.providers()
        do {
            let m = try PiyaBackend.modelSettings()
            original = m
            provider = m.provider
            apiKey = m.apiKey
            baseUrl = m.baseUrl
            model = m.model
            embedProvider = m.embedProvider.isEmpty ? "none" : m.embedProvider
            embedApiKey = m.embedApiKey
            embedBaseUrl = m.embedBaseUrl
            embeddingModel = m.embeddingModel
            status = ""
            chatStatus = nil
            embedStatus = nil
            models = []
            embedModels = []
            if !(info?.needsKey ?? true) || !apiKey.isEmpty { refreshModels() }
        } catch {
            status = error.localizedDescription
        }
    }

    private func switchChatProvider(to id: String) {
        guard let p = providers.first(where: { $0.id == id }), id != original?.provider || apiKey.isEmpty else {
            if let o = original, id == o.provider {
                apiKey = o.apiKey; baseUrl = o.baseUrl; model = o.model
            }
            return
        }
        apiKey = p.savedKey
        baseUrl = p.savedBaseUrl.isEmpty ? p.defaultBaseUrl : p.savedBaseUrl
        model = p.suggestedModel
        models = []
        chatStatus = nil
        if !p.needsKey || !apiKey.isEmpty { refreshModels() }
    }

    private func switchEmbedProvider(to id: String) {
        embedModels = []
        embedStatus = nil
        guard let p = providers.first(where: { $0.id == id }) else { return }
        if let o = original, o.embedProvider == id {
            embedApiKey = o.embedApiKey; embedBaseUrl = o.embedBaseUrl; embeddingModel = o.embeddingModel
        } else {
            embedApiKey = p.savedKey
            embedBaseUrl = p.savedBaseUrl.isEmpty ? p.defaultBaseUrl : p.savedBaseUrl
            embeddingModel = p.defaultEmbeddingModel
        }
    }

    /// Connection used for embeddings: the chat one when providers match.
    private var embedConnection: (key: String, url: String) {
        embedSharesChatKey ? (apiKey, baseUrl) : (embedApiKey, embedBaseUrl)
    }

    private func refreshModels() {
        let (p, k, u) = (provider, apiKey, baseUrl)
        loadingModels = true
        Task.detached {
            let result = Result { try PiyaBackend.listModels(provider: p, apiKey: k, baseUrl: u, embeddings: false) }
            await MainActor.run {
                loadingModels = false
                switch result {
                case .success(let list):
                    models = list
                    if model.isEmpty, let first = list.first { model = first }
                    chatStatus = (true, "\(list.count) models available.")
                case .failure(let error):
                    models = []
                    chatStatus = (false, error.localizedDescription)
                }
            }
        }
    }

    private func refreshEmbedModels() {
        let p = embedProvider
        let (k, u) = embedConnection
        Task.detached {
            let result = Result { try PiyaBackend.listModels(provider: p, apiKey: k, baseUrl: u, embeddings: true) }
            await MainActor.run {
                switch result {
                case .success(let list):
                    embedModels = list
                    embedStatus = (true, list.isEmpty ? "No embeddings models listed — type one." : "\(list.count) embeddings models.")
                case .failure(let error):
                    embedStatus = (false, error.localizedDescription)
                }
            }
        }
    }

    private func testChat() {
        let (p, k, u, m) = (provider, apiKey, baseUrl, model)
        chatStatus = (true, "Testing…")
        Task.detached {
            let probe = PiyaBackend.testModel(provider: p, apiKey: k, baseUrl: u, model: m, embeddings: false)
            await MainActor.run { chatStatus = (probe.ok, probe.message) }
        }
    }

    private func testEmbeddings() {
        let (p, m) = (embedProvider, embeddingModel)
        let (k, u) = embedConnection
        embedStatus = (true, "Testing…")
        Task.detached {
            let probe = PiyaBackend.testModel(provider: p, apiKey: k, baseUrl: u, model: m, embeddings: true)
            await MainActor.run { embedStatus = (probe.ok, probe.message) }
        }
    }

    private func save() {
        let (ek, eu) = embedConnection
        let settings = FfiModelSettings(
            provider: provider,
            apiKey: apiKey.trimmingCharacters(in: .whitespacesAndNewlines),
            baseUrl: baseUrl.trimmingCharacters(in: .whitespacesAndNewlines),
            model: model.trimmingCharacters(in: .whitespacesAndNewlines),
            embedProvider: embedProvider,
            embedApiKey: ek.trimmingCharacters(in: .whitespacesAndNewlines),
            embedBaseUrl: eu.trimmingCharacters(in: .whitespacesAndNewlines),
            embeddingModel: embedProvider == "none" ? "" : embeddingModel.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        do {
            try PiyaBackend.saveModelSettings(settings)
            let reindex = original.map {
                $0.embedProvider != settings.embedProvider || $0.embeddingModel != settings.embeddingModel
            } ?? false
            original = settings
            status = "Saved — \(info?.label ?? provider) · \(settings.model) applies to the next message."
                + (reindex && settings.embedProvider != "none" ? " Memories will re-index with the new embeddings model." : "")
        } catch {
            status = error.localizedDescription
        }
    }
}

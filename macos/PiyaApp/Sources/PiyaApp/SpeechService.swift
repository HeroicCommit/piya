import AVFoundation
import Foundation
import PDFKit
import Speech

/// On-device Apple Speech dictation for the Hummingbird composer, plus local text
/// extraction for attached PDFs and audio files.
@MainActor
final class SpeechDictationController: ObservableObject {
    @Published private(set) var isRecording = false
    @Published private(set) var liveTranscript = ""
    @Published var errorMessage: String?

    private var audioEngine: AVAudioEngine?
    private var recognitionRequest: SFSpeechAudioBufferRecognitionRequest?
    private var recognitionTask: SFSpeechRecognitionTask?
    private var onTranscript: ((String) -> Void)?

    func toggle(onTranscript: @escaping (String) -> Void) {
        if isRecording {
            stop()
        } else {
            start(onTranscript: onTranscript)
        }
    }

    func start(onTranscript: @escaping (String) -> Void) {
        guard !isRecording else { return }
        self.onTranscript = onTranscript
        self.liveTranscript = ""
        self.errorMessage = nil

        Task { @MainActor in
            let authorized = await Self.ensurePermissions()
            guard authorized else {
                self.errorMessage =
                    "Allow Microphone and Speech Recognition for Piya in System Settings → Privacy & Security."
                return
            }
            self.beginRecordingSession()
        }
    }

    func stop() {
        guard isRecording else { return }
        isRecording = false
        audioEngine?.inputNode.removeTap(onBus: 0)
        audioEngine?.stop()
        recognitionRequest?.endAudio()
        recognitionTask?.finish()
        audioEngine = nil
        recognitionRequest = nil
        recognitionTask = nil
    }

    private func beginRecordingSession() {
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else {
            errorMessage = "Apple Speech Recognition is not currently available for your locale."
            return
        }

        let engine = AVAudioEngine()
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        if recognizer.supportsOnDeviceRecognition {
            request.requiresOnDeviceRecognition = true
        }

        let inputNode = engine.inputNode
        let format = inputNode.outputFormat(forBus: 0)
        guard format.channelCount > 0 else {
            errorMessage = "No active microphone input device found."
            return
        }

        inputNode.removeTap(onBus: 0)
        inputNode.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak request] buffer, _ in
            request?.append(buffer)
        }

        do {
            engine.prepare()
            try engine.start()
        } catch {
            inputNode.removeTap(onBus: 0)
            errorMessage = "Could not start microphone: \(error.localizedDescription)"
            return
        }

        self.audioEngine = engine
        self.recognitionRequest = request
        self.isRecording = true

        self.recognitionTask = recognizer.recognitionTask(with: request) { [weak self] result, error in
            let text = result?.bestTranscription.formattedString
            let isFinal = result?.isFinal ?? false
            let nsErr = error as NSError?
            Task { @MainActor [weak self] in
                guard let self else { return }
                if let text, !text.isEmpty {
                    self.liveTranscript = text
                    self.onTranscript?(text)
                }
                if isFinal {
                    self.stop()
                } else if let nsErr, self.isRecording {
                    // Ignore normal cancellation codes when user taps stop.
                    if nsErr.code != 216 && nsErr.code != 301 && nsErr.code != 1110 {
                        self.errorMessage = nsErr.localizedDescription
                    }
                    self.stop()
                }
            }
        }
    }

    static func ensurePermissions() async -> Bool {
        let micGranted: Bool = await withCheckedContinuation { cont in
            switch AVCaptureDevice.authorizationStatus(for: .audio) {
            case .authorized:
                cont.resume(returning: true)
            case .notDetermined:
                AVCaptureDevice.requestAccess(for: .audio) { ok in
                    cont.resume(returning: ok)
                }
            default:
                cont.resume(returning: false)
            }
        }
        guard micGranted else { return false }

        let speechGranted: Bool = await withCheckedContinuation { cont in
            switch SFSpeechRecognizer.authorizationStatus() {
            case .authorized:
                cont.resume(returning: true)
            case .notDetermined:
                SFSpeechRecognizer.requestAuthorization { status in
                    cont.resume(returning: status == .authorized)
                }
            default:
                cont.resume(returning: false)
            }
        }
        return speechGranted
    }
}

/// Extracts selectable text from PDFs (via PDFKit) and transcribes audio files
/// on-device via `SFSpeechRecognizer` before staging in Rust.
enum MediaExtractor {
    private static let audioExtensions: Set<String> = [
        "m4a", "mp3", "wav", "aac", "caf", "ogg",
    ]

    static func extractText(from url: URL) async -> String {
        let ext = url.pathExtension.lowercased()
        if ext == "pdf" {
            return extractPdfText(from: url)
        }
        if audioExtensions.contains(ext) {
            return await transcribeAudioFile(at: url)
        }
        return ""
    }

    private static func extractPdfText(from url: URL) -> String {
        guard let doc = PDFDocument(url: url) else { return "" }
        var pages: [String] = []
        let limit = min(doc.pageCount, 80)
        for idx in 0..<limit {
            if let page = doc.page(at: idx)?.string?.trimmingCharacters(in: .whitespacesAndNewlines),
               !page.isEmpty
            {
                pages.append(page)
            }
        }
        return pages.joined(separator: "\n\n")
    }

    private static func transcribeAudioFile(at url: URL) async -> String {
        let speechOK: Bool = await withCheckedContinuation { cont in
            switch SFSpeechRecognizer.authorizationStatus() {
            case .authorized:
                cont.resume(returning: true)
            case .notDetermined:
                SFSpeechRecognizer.requestAuthorization { status in
                    cont.resume(returning: status == .authorized)
                }
            default:
                cont.resume(returning: false)
            }
        }
        guard speechOK, let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else {
            return ""
        }
        if recognizer.supportsOnDeviceRecognition {
            let onDevice = await runFileRecognition(recognizer: recognizer, url: url, onDevice: true)
            if !onDevice.isEmpty {
                return onDevice
            }
        }
        return await runFileRecognition(recognizer: recognizer, url: url, onDevice: false)
    }

    private static func runFileRecognition(
        recognizer: SFSpeechRecognizer,
        url: URL,
        onDevice: Bool
    ) async -> String {
        await withCheckedContinuation { cont in
            let req = SFSpeechURLRecognitionRequest(url: url)
            req.shouldReportPartialResults = false
            req.requiresOnDeviceRecognition = onDevice
            var finished = false
            _ = recognizer.recognitionTask(with: req) { result, error in
                guard !finished else { return }
                if let result, result.isFinal {
                    finished = true
                    cont.resume(returning: result.bestTranscription.formattedString)
                } else if error != nil {
                    finished = true
                    cont.resume(returning: result?.bestTranscription.formattedString ?? "")
                }
            }
        }
    }
}

import AppKit
import Foundation

/// Tails the core's log file.
///
/// The React panel listened to a webview log stream that the core emits only
/// while debug mode is on. A native shell has no webview, so the same lines
/// are read from the file the core writes: `sona.log` in the directory
/// `get_log_dir_path` names. Reading starts at the end of the file, so only
/// lines written while the panel is open appear, which is what the old panel
/// promised too. A rotation (the core keeps one file of 500 KB) shortens the
/// file, and a shorter file than the last offset is the signal to start over.
final class LogTailReader: @unchecked Sendable {
    /// The cadence the old panel flushed at, so a burst of logging can never
    /// cost a render per line.
    private static let interval: DispatchTimeInterval = .milliseconds(250)
    /// The most one tick will take in, so a long-running app that logged
    /// megabytes while the panel was closed cannot arrive at once.
    private static let maxChunk = 256 * 1024

    private let url: URL
    private let queue = DispatchQueue(label: "sona.log-tail", qos: .utility)
    private let deliver: @MainActor ([LogLine]) -> Void
    private var timer: DispatchSourceTimer?
    private var offset: UInt64 = 0
    private var carried = ""
    private var nextID = 0

    init(directory: String, fileName: String = "sona.log", deliver: @escaping @MainActor ([LogLine]) -> Void) {
        url = URL(fileURLWithPath: directory).appending(path: fileName)
        self.deliver = deliver
    }

    /// Starts at the current end of the file and polls for what is appended.
    func start() {
        queue.async { [self] in
            offset = endOfFile() ?? 0
            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now() + Self.interval, repeating: Self.interval)
            timer.setEventHandler { [weak self] in self?.poll() }
            self.timer = timer
            timer.resume()
        }
    }

    func stop() {
        queue.async { [self] in
            timer?.cancel()
            timer = nil
        }
    }

    private func endOfFile() -> UInt64? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        return try? handle.seekToEnd()
    }

    private func poll() {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return }
        defer { try? handle.close() }
        guard let end = try? handle.seekToEnd() else { return }
        if end < offset {
            // Rotated or truncated: the bytes behind the old offset are gone.
            offset = 0
            carried = ""
        }
        guard end > offset else { return }
        let wanted = min(Int(end - offset), Self.maxChunk)
        guard (try? handle.seek(toOffset: offset)) != nil,
              let data = try? handle.read(upToCount: wanted), !data.isEmpty
        else { return }
        offset += UInt64(data.count)
        var text = carried + String(decoding: data, as: UTF8.self)
        carried = ""
        if !text.hasSuffix("\n") {
            // A line the core is still writing waits for the rest of itself.
            if let lastBreak = text.lastIndex(of: "\n") {
                carried = String(text[text.index(after: lastBreak)...])
                text = String(text[..<text.index(after: lastBreak)])
            } else {
                carried = text
                return
            }
        }
        var parsed: [LogLine] = []
        for line in text.components(separatedBy: "\n") where !line.isEmpty {
            parsed.append(Self.parse(line, id: nextID))
            nextID += 1
        }
        guard !parsed.isEmpty else { return }
        let delivery = deliver
        DispatchQueue.main.async {
            MainActor.assumeIsolated { delivery(parsed) }
        }
    }

    private static let clock: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss"
        return formatter
    }()

    /// `[2026-09-12][21:35:10][sona_app_lib::actions][DEBUG] message`, as
    /// tauri-plugin-log writes it. A line that does not carry those brackets
    /// (the second line of a multi-line message) keeps its whole text.
    static func parse(_ line: String, id: Int) -> LogLine {
        var fields: [String] = []
        var rest = Substring(line)
        while rest.first == "[", let close = rest.firstIndex(of: "]") {
            fields.append(String(rest[rest.index(after: rest.startIndex)..<close]))
            rest = rest[rest.index(after: close)...]
        }
        let level = fields.compactMap { LogLevel(logTag: $0) }.first
        let time = fields.first { $0.count == 8 && $0.filter { $0 == ":" }.count == 2 }
        let target = fields.count >= 3 ? fields[fields.count - 2] : nil
        let message = fields.isEmpty
            ? line
            : String(rest).trimmingCharacters(in: .whitespaces)
        return LogLine(
            id: id,
            time: time ?? clock.string(from: Date()),
            level: level,
            target: level == nil ? nil : target,
            message: message
        )
    }
}

/// Instrumentation: the density is the point.
///
/// What this store does not hold is the capture group's microphone and sound
/// rows, which belong to the settings slice; this one owns the recording
/// buffer, the word-correction threshold, the keyboard diagnostic, the release
/// note preview, the log level and the live log.
@MainActor
@Observable
final class DebugStore {
    /// The most lines kept in memory and rendered at once.
    static let lineCap = 1000
    /// The core's own default (`settings.rs`), used until the defaults arrive.
    static let defaultWordCorrectionThreshold = 0.18

    private(set) var settings: DebugSettingsSnapshot?
    /// The core's defaults, which is what a row resets to.
    private(set) var defaults: DebugSettingsSnapshot?
    private(set) var logDirectory: String?
    private(set) var lines: [LogLine] = []
    private(set) var paused = false
    /// The least severe level the viewer shows. Lines the format does not
    /// explain have no level and are always shown.
    private(set) var filter: LogLevel = .trace
    private(set) var diagnosticRunning = false
    private(set) var diagnosticReport: KeyboardDiagnosticReport?
    /// The core's own reason, not a sentence of ours.
    private(set) var diagnosticError: String?
    private(set) var copied = false
    /// True when the selected model is handed the vocabulary as its decode
    /// prompt, which is what makes the correction threshold unread.
    private(set) var promptedModel = false
    private(set) var error: String?

    /// The release-note sheet the Debug page previews. Its own instance, so a
    /// preview never touches the version the launch gate would record.
    let whatsNew: WhatsNewStore

    /// Lines that arrived while the stream was paused, shown on resume.
    @ObservationIgnored private var pending: [LogLine] = []
    @ObservationIgnored private var tail: LogTailReader?
    @ObservationIgnored private var copyReset: Task<Void, Never>?
    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        whatsNew = WhatsNewStore(core: core)
        core.observe(CoreEvent.aboutSettingsChanged) { [weak self] _ in self?.reloadSettings() }
        core.observe(CoreEvent.modelsUpdated) { [weak self] _ in self?.reloadModel() }
        core.observe(CoreEvent.modelStateChanged) { [weak self] _ in self?.reloadModel() }
    }

    /// The first load: settings, the core's defaults, the selected model, and
    /// the log file the viewer tails.
    func start() async {
        await loadSettings()
        await loadDefaults()
        await loadModel()
        await startTail()
    }

    // MARK: - Settings

    var debugMode: Bool { settings?.debugMode ?? false }
    var logLevel: LogLevel { settings?.logLevel ?? .debug }
    var recordingBufferMs: Int { settings?.extraRecordingBufferMs ?? 0 }
    var wordCorrectionThreshold: Double {
        settings?.wordCorrectionThreshold ?? Self.defaultWordCorrectionThreshold
    }

    var defaultRecordingBufferMs: Int { defaults?.extraRecordingBufferMs ?? 0 }
    var defaultThreshold: Double {
        defaults?.wordCorrectionThreshold ?? Self.defaultWordCorrectionThreshold
    }

    private func loadSettings() async {
        do {
            settings = try await core.request("get_app_settings")
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func loadDefaults() async {
        do {
            defaults = try await core.request("get_default_settings")
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func reloadSettings() {
        Task { await loadSettings() }
    }

    func setLogLevel(_ level: LogLevel) {
        guard level != logLevel else { return }
        call { try await self.core.request("set_log_level", ["level": level.rawValue]) }
    }

    func setRecordingBuffer(_ ms: Int) {
        guard ms != recordingBufferMs else { return }
        call { try await self.core.request("change_extra_recording_buffer_setting", ["ms": ms]) }
    }

    func resetRecordingBuffer() {
        setRecordingBuffer(defaultRecordingBufferMs)
    }

    func setWordCorrectionThreshold(_ threshold: Double) {
        guard threshold != wordCorrectionThreshold else { return }
        call {
            try await self.core.request(
                "change_word_correction_threshold_setting", ["threshold": threshold]
            )
        }
    }

    func resetWordCorrectionThreshold() {
        setWordCorrectionThreshold(defaultThreshold)
    }

    /// Turning this off is what closes the Debug page: the same setting the
    /// ⌘⇧D chord flipped in the old app.
    func setDebugMode(_ enabled: Bool) {
        call { try await self.core.request("change_debug_mode_setting", ["enabled": enabled]) }
    }

    func toggleDebugMode() {
        setDebugMode(!debugMode)
    }

    // MARK: - The selected model

    private func loadModel() async {
        do {
            let selected: String = try await core.request("get_current_model")
            let id = selected.isEmpty ? (settings?.selectedModel ?? "") : selected
            guard !id.isEmpty else {
                promptedModel = false
                return
            }
            let models: [WordCorrectionModel] = try await core.request("get_available_models")
            let name = models.first { $0.id == id }?.name ?? ""
            promptedModel = WordCorrectionFamily.takesVocabularyAsPrompt(id: id, name: name)
        } catch {
            // A model the shell could not read leaves the row live rather than
            // dimming it on a guess.
            promptedModel = false
        }
    }

    private func reloadModel() {
        Task { await loadModel() }
    }

    // MARK: - Keyboard diagnostic

    var diagnosticVerdict: KeyboardDiagnosticVerdict? {
        diagnosticReport.map(KeyboardDiagnosticVerdict.init)
    }

    /// Opens a short-lived listener and tallies how many key-down, key-up,
    /// modifier and mouse events reach Sona — never which keys.
    func runDiagnostic() {
        guard !diagnosticRunning else { return }
        diagnosticRunning = true
        diagnosticReport = nil
        diagnosticError = nil
        Task {
            do {
                let report: KeyboardDiagnosticReport = try await core.request(
                    "run_keyboard_diagnostic", ["durationSecs": 10]
                )
                diagnosticReport = report
                error = nil
            } catch {
                diagnosticError = error.localizedDescription
            }
            diagnosticRunning = false
        }
    }

    /// The message the diagnostic row prints for a failure.
    var diagnosticMessage: String? {
        guard let diagnosticError else { return nil }
        if diagnosticError == "permission_denied" {
            return "Sona cannot read keyboard events. Allow Input Monitoring, then try again."
        }
        return "The diagnostic could not run: \(diagnosticError)"
    }

    // MARK: - Live logs

    /// The file the viewer reads, named so a reader knows what they are seeing.
    var logFilePath: String? {
        logDirectory.map { $0 + "/sona.log" }
    }

    var visibleLines: [LogLine] {
        guard filter != .trace else { return lines }
        return lines.filter { line in
            guard let level = line.level else { return true }
            return level.severity >= filter.severity
        }
    }

    private func startTail() async {
        do {
            let directory: String = try await core.request("get_log_dir_path")
            logDirectory = directory
            let tail = LogTailReader(directory: directory) { [weak self] lines in
                self?.append(lines)
            }
            self.tail = tail
            tail.start()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func append(_ incoming: [LogLine]) {
        if paused {
            pending.append(contentsOf: incoming)
            if pending.count > Self.lineCap {
                pending.removeFirst(pending.count - Self.lineCap)
            }
            return
        }
        lines.append(contentsOf: incoming)
        if lines.count > Self.lineCap {
            lines.removeFirst(lines.count - Self.lineCap)
        }
    }

    /// Whether the stream is running is printed once, by the control that
    /// changes it: "Pause" can only mean it is live.
    func togglePaused() {
        paused.toggle()
        guard !paused, !pending.isEmpty else { return }
        let resumed = pending
        pending = []
        append(resumed)
    }

    func setFilter(_ level: LogLevel) {
        filter = level
    }

    func clearLines() {
        lines = []
        pending = []
    }

    func copyLines() {
        let text = visibleLines.map(\.transcript).joined(separator: "\n")
        guard !text.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        copied = true
        copyReset?.cancel()
        copyReset = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(1500))
            guard !Task.isCancelled else { return }
            self?.copied = false
        }
    }

    // MARK: - Release note preview

    /// Opens the newest bundled release note without marking it as seen.
    func previewReleaseNote() {
        whatsNew.previewLatest()
    }

    // MARK: - Plumbing

    private func call(_ work: @escaping () async throws -> Void) {
        Task {
            do {
                try await work()
                error = nil
                await loadSettings()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

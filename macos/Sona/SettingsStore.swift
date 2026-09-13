import AppKit
import Foundation
import Observation

/// Everything the settings pages read and write.
///
/// The core owns the settings file; this holds the last copy it handed over
/// and nothing else. A row writes through its own command and the record is
/// read back, so what a control shows is what the core stored — not what the
/// click meant. `settings-changed` fires for every write from anywhere (this
/// window, the tray, an agent proposal) and carries the name of one setting,
/// which is enough to know a re-read is due and never enough to skip it.
@MainActor
@Observable
final class SettingsStore {
    /// The settings record. Starts at the core's own defaults so no row
    /// claims "off" for something that ships on, before the first read.
    private(set) var settings = AppSettings()
    /// `get_default_settings`, which is what a reset row writes back.
    private(set) var defaults = AppSettings()
    private(set) var loaded = false
    private(set) var microphones: [AudioDevice] = []
    /// Channels on the selected microphone. One means no channel row.
    private(set) var channelCount = 1
    /// Whether this machine has a lid, which is the only thing the clamshell
    /// microphone row is for.
    private(set) var laptop = false
    private(set) var accelerators = AcceleratorOptions.empty
    private(set) var customSounds = SoundCustomFiles.none
    /// Model capabilities by id, for the language and translation rows.
    private(set) var capabilities: [String: SettingsModelCapability] = [:]
    /// The last thing the core refused to do.
    private(set) var error: String?
    /// Something that succeeded and is worth saying anyway: the chords a
    /// keyboard-implementation switch had to drop.
    private(set) var notice: String?
    /// Rows with a write in flight, by the key the row disables on.
    private(set) var busy: Set<String> = []

    /// Called after the core accepts an autostart change. The login item
    /// registration is the shell's own: only the app bundle knows itself.
    var onAutostartChanged: (Bool) -> Void = { _ in }

    @ObservationIgnored let core: Core
    /// The shortcut recorder's ear, while one is recording. Registered once
    /// in `init` and pointed at whichever row is capturing.
    @ObservationIgnored var onHandyKeys: ((HandyKeysEvent) -> Void)?

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.settingsChanged) { [weak self] line in
            guard let self else { return }
            let change: SettingsChangeNotice? = try? Core.payload(line)
            Task { await self.reload(after: change?.setting) }
        }
        core.observe(CoreEvent.handyKeys) { [weak self] line in
            guard let self, let event: HandyKeysEvent = try? Core.payload(line) else { return }
            onHandyKeys?(event)
        }
        core.observe(CoreEvent.modelsUpdated) { [weak self] _ in
            guard let self else { return }
            Task { await self.refreshCapabilities() }
        }
    }

    /// The payload of `settings-changed`: which setting was written.
    private struct SettingsChangeNotice: Decodable {
        let setting: String?
    }

    // MARK: - Reading

    /// The first load. Everything a settings page needs that is not in the
    /// settings record itself is read once here, and re-read when the thing
    /// it describes changes.
    func start() async {
        await refresh()
        loaded = true
        await ask { self.defaults = try await self.core.request("get_default_settings") }
        await refreshMicrophones()
        await refreshChannels()
        await refreshCustomSounds()
        await refreshCapabilities()
        await ask { self.accelerators = try await self.core.request("get_available_accelerators") }
        await ask { self.laptop = try await self.core.request("is_laptop") }
    }

    func refresh() async {
        await ask { self.settings = try await self.core.request("get_app_settings") }
    }

    /// A write landed somewhere. Re-read the record, and the enumeration the
    /// written setting invalidates.
    private func reload(after setting: String?) async {
        await refresh()
        switch setting {
        case "selected_microphone":
            await refreshMicrophones()
            await refreshChannels()
        case "clamshell_microphone":
            await refreshMicrophones()
        case "sound_theme":
            await refreshCustomSounds()
        case "selected_model":
            await refreshCapabilities()
        default:
            break
        }
    }

    func refreshMicrophones() async {
        await ask { self.microphones = try await self.core.request("get_available_microphones") }
    }

    /// How many channels the selected microphone has. The device the core
    /// enumerates as "Default" is stored as the `default` sentinel.
    func refreshChannels() async {
        let device = Self.deviceName(settings.selectedMicrophone)
        channelCount = 1
        await ask {
            self.channelCount = try await self.core.request("get_microphone_channels", ["deviceName": JSONValue.string(device)])
        }
    }

    func refreshCustomSounds() async {
        await ask { self.customSounds = try await self.core.request("check_custom_sounds") }
    }

    func refreshCapabilities() async {
        await ask {
            let models: [SettingsModelCapability] = try await self.core.request("get_available_models")
            self.capabilities = Dictionary(models.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        }
    }

    func refreshAccelerators() async {
        await ask { self.accelerators = try await self.core.request("get_available_accelerators") }
    }

    // MARK: - What the rows read

    /// The model the next dictation will use, as far as its capabilities go.
    var model: SettingsModelCapability? { capabilities[settings.selectedModel] }

    var supportedLanguages: [String] { model?.supportedLanguages ?? [] }

    var detectsLanguage: Bool { model?.supportsLanguageDetection ?? true }

    var supportsTranslation: Bool { model?.supportsTranslation ?? false }

    /// The language in force: the stored intent resolved against what the
    /// loaded model can actually recognize.
    var effectiveLanguage: String {
        LanguageCatalog.effective(
            intent: settings.selectedLanguage,
            supported: supportedLanguages,
            detects: detectsLanguage
        )
    }

    var languageChoices: [LanguageOption] {
        LanguageCatalog.available(supported: supportedLanguages, detects: detectsLanguage)
    }

    /// "Default" is what every enumeration calls the device the system
    /// picks, and `default` is what the settings file stores for it.
    static func deviceName(_ stored: String?) -> String {
        guard let stored, stored != "Default" else { return "default" }
        return stored
    }

    static func deviceLabel(_ stored: String?) -> String {
        guard let stored, !stored.isEmpty, stored != "default" else { return "Default" }
        return stored
    }

    func binding(_ id: String) -> BindingRecord? { settings.bindings[id] }

    func isBusy(_ key: String) -> Bool { busy.contains(key) }

    func clearNotice() { notice = nil }

    /// The transcribe.cpp menu: Auto, then one row per GPU, then CPU. The
    /// device is part of the choice, not a second row.
    var transcribeChoices: [AcceleratorChoice] {
        var rows: [AcceleratorChoice] = []
        if accelerators.transcribe.contains("auto") {
            rows.append(AcceleratorChoice(accelerator: .auto, device: nil, label: "Auto"))
        }
        if accelerators.transcribe.contains("gpu") {
            for device in accelerators.gpuDevices {
                rows.append(AcceleratorChoice(accelerator: .gpu, device: device.id, label: device.label))
            }
        }
        if accelerators.transcribe.contains("cpu") {
            rows.append(AcceleratorChoice(accelerator: .cpu, device: nil, label: "CPU"))
        }
        return rows
    }

    /// The row that is selected now, or the first one when the stored pair
    /// names a device this machine no longer has.
    var transcribeChoice: AcceleratorChoice? {
        let rows = transcribeChoices
        let stored = rows.first { row in
            switch settings.transcribeAccelerator {
            case .cpu: row.accelerator == .cpu
            case .gpu: row.accelerator == .gpu && row.device == settings.transcribeGpuDevice
            case .auto: row.accelerator == .auto
            }
        }
        return stored ?? rows.first
    }

    /// The ONNX providers this build can store, with `auto` always offered.
    var ortChoices: [AcceleratorOrt] {
        let reported = accelerators.ort.contains("auto") ? accelerators.ort : ["auto"] + accelerators.ort
        return reported.compactMap(AcceleratorOrt.init(rawValue:))
    }

    // MARK: - Writing

    /// Every write: show it at once, send it, then let the core's own record
    /// correct the row. A refusal leaves the reason under the page title and
    /// the re-read puts the control back where the core still has it.
    private func write<T>(
        _ field: WritableKeyPath<AppSettings, T>,
        _ value: T,
        _ method: String,
        _ params: [String: JSONValue],
        key: String
    ) async {
        settings[keyPath: field] = value
        await send(method, params, key: key)
        await refresh()
    }

    /// A command with nothing in the settings record to show first.
    private func send(_ method: String, _ params: [String: JSONValue]? = nil, key: String) async {
        busy.insert(key)
        do {
            if let params {
                try await core.request(method, params)
            } else {
                try await core.request(method)
            }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        busy.remove(key)
    }

    /// A read. A failed read says so and leaves the last good value.
    private func ask(_ body: () async throws -> Void) async {
        do {
            try await body()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    // MARK: - Essentials

    func setPushToTalk(_ enabled: Bool) async {
        await write(\.pushToTalk, enabled, "change_ptt_setting", ["enabled": .bool(enabled)], key: "push_to_talk")
    }

    func setMicrophone(_ name: String) async {
        await write(
            \.selectedMicrophone, name, "set_selected_microphone",
            ["deviceName": .string(Self.deviceName(name))], key: "selected_microphone"
        )
        await refreshChannels()
    }

    func resetMicrophone() async {
        await setMicrophone(Self.deviceLabel(defaults.selectedMicrophone))
    }

    func setClamshellMicrophone(_ name: String) async {
        await write(
            \.clamshellMicrophone, name, "set_clamshell_microphone",
            ["deviceName": .string(Self.deviceName(name))], key: "clamshell_microphone"
        )
    }

    func resetClamshellMicrophone() async {
        await setClamshellMicrophone(Self.deviceLabel(defaults.clamshellMicrophone))
    }

    func setAlwaysOnMicrophone(_ enabled: Bool) async {
        await write(
            \.alwaysOnMicrophone, enabled, "update_microphone_mode",
            ["alwaysOn": .bool(enabled)], key: "always_on_microphone"
        )
    }

    /// `nil` averages every channel, which is what the recorder falls back
    /// to when the stored channel is gone.
    func setChannel(_ channel: Int?) async {
        await write(
            \.selectedChannel, channel, "set_selected_channel",
            ["channel": channel.map { JSONValue.number(Double($0)) } ?? .null], key: "selected_channel"
        )
    }

    func setLanguage(_ code: String) async {
        await write(
            \.selectedLanguage, code, "change_selected_language_setting",
            ["language": .string(code)], key: "selected_language"
        )
    }

    func resetLanguage() async {
        await setLanguage(defaults.selectedLanguage)
    }

    func setAudioFeedback(_ enabled: Bool) async {
        await write(
            \.audioFeedback, enabled, "change_audio_feedback_setting",
            ["enabled": .bool(enabled)], key: "audio_feedback"
        )
    }

    func setAudioFeedbackVolume(_ volume: Double) async {
        await write(
            \.audioFeedbackVolume, volume, "change_audio_feedback_volume_setting",
            ["volume": .number(volume)], key: "audio_feedback_volume"
        )
    }

    func setSoundTheme(_ theme: SoundTheme) async {
        await write(
            \.soundTheme, theme, "change_sound_theme_setting",
            ["theme": .string(theme.rawValue)], key: "sound_theme"
        )
        await refreshCustomSounds()
    }

    /// `start` or `stop`, played through the current theme.
    func playTestSound(_ kind: String) async {
        await send("play_test_sound", ["soundType": .string(kind)], key: "test_sound")
    }

    func setAutostart(_ enabled: Bool) async {
        await write(
            \.autostartEnabled, enabled, "change_autostart_setting",
            ["enabled": .bool(enabled)], key: "autostart_enabled"
        )
        if error == nil { onAutostartChanged(enabled) }
    }

    // MARK: - Dictation

    func setCommandMode(_ enabled: Bool) async {
        await write(
            \.commandModeEnabled, enabled, "change_command_mode_enabled_setting",
            ["enabled": .bool(enabled)], key: "command_mode_enabled"
        )
    }

    func setSpelling(_ spelling: DictationSpelling) async {
        await write(
            \.englishSpelling, spelling, "change_english_spelling_setting",
            ["spelling": .string(spelling.rawValue)], key: "english_spelling"
        )
    }

    func setTranslateToEnglish(_ enabled: Bool) async {
        await write(
            \.translateToEnglish, enabled, "change_translate_to_english_setting",
            ["enabled": .bool(enabled)], key: "translate_to_english"
        )
    }

    func setOverlayStyle(_ style: OverlayStyle) async {
        await write(
            \.overlayStyle, style, "change_overlay_style_setting",
            ["style": .string(style.rawValue)], key: "overlay_style"
        )
    }

    func setOverlayPosition(_ position: OverlayPosition) async {
        await write(
            \.overlayPosition, position, "change_overlay_position_setting",
            ["position": .string(position.rawValue)], key: "overlay_position"
        )
    }

    /// The pill has its own commands rather than the settings writer: it has
    /// to appear now, not at the next dictation.
    func setHudPillEnabled(_ enabled: Bool) async {
        await write(
            \.hudPillEnabled, enabled, "set_hud_pill_enabled",
            ["enabled": .bool(enabled)], key: "hud_pill_enabled"
        )
    }

    func setHudPillPosition(_ position: OverlayPosition) async {
        await write(
            \.hudPillPosition, position, "set_hud_pill_position",
            ["position": .string(position.rawValue)], key: "hud_pill_position"
        )
    }

    func setUnloadTimeout(_ timeout: DictationUnloadTimeout) async {
        await write(
            \.modelUnloadTimeout, timeout, "set_model_unload_timeout",
            ["timeout": .string(timeout.rawValue)], key: "model_unload_timeout"
        )
    }

    // MARK: - Experimental

    func setExperimental(_ enabled: Bool) async {
        await write(
            \.experimentalEnabled, enabled, "change_experimental_enabled_setting",
            ["enabled": .bool(enabled)], key: "experimental_enabled"
        )
    }

    func setLazyStreamClose(_ enabled: Bool) async {
        await write(
            \.lazyStreamClose, enabled, "change_lazy_stream_close_setting",
            ["enabled": .bool(enabled)], key: "lazy_stream_close"
        )
    }

    /// Switching layers can drop a chord the other one cannot express, and
    /// the core says which. Saying nothing would leave a shortcut silently
    /// back at its default.
    func setKeyboardImplementation(_ implementation: KeyboardImplementation) async {
        guard implementation != settings.keyboardImplementation else { return }
        let key = "keyboard_implementation"
        busy.insert(key)
        do {
            let change: KeyboardChange = try await core.request(
                "change_keyboard_implementation_setting",
                ["implementation": JSONValue.string(implementation.rawValue)]
            )
            error = nil
            notice = change.resetBindings.isEmpty
                ? nil
                : "Keyboard shortcuts were incompatible and reset to defaults."
        } catch {
            self.error = error.localizedDescription
        }
        busy.remove(key)
        await refresh()
    }

    /// The device is written first: `gpu` with no device is normalized back
    /// to `auto` by the core.
    func setTranscribeChoice(_ choice: AcceleratorChoice) async {
        await write(
            \.transcribeGpuDevice, choice.device, "change_transcribe_gpu_device",
            ["device": choice.device.map(JSONValue.string) ?? .null], key: "transcribe_gpu_device"
        )
        await write(
            \.transcribeAccelerator, choice.accelerator, "change_transcribe_accelerator_setting",
            ["accelerator": .string(choice.accelerator.rawValue)], key: "transcribe_accelerator"
        )
    }

    func setOrtAccelerator(_ accelerator: AcceleratorOrt) async {
        await write(
            \.ortAccelerator, accelerator, "change_ort_accelerator_setting",
            ["accelerator": .string(accelerator.rawValue)], key: "ort_accelerator"
        )
    }

    // MARK: - Shortcuts

    /// Store one chord. False means the core refused it and the row should
    /// put back what it was showing.
    @discardableResult
    func changeBinding(_ id: String, chord: String) async -> Bool {
        let key = "binding_\(id)"
        busy.insert(key)
        var stored = false
        do {
            let response: BindingChange = try await core.request(
                "change_binding", ["id": JSONValue.string(id), "binding": .string(chord)]
            )
            stored = response.success
            error = response.success
                ? nil
                : "Couldn't set the shortcut: \(response.error ?? "the core refused it")"
        } catch {
            self.error = "Couldn't set the shortcut: \(error.localizedDescription)"
        }
        busy.remove(key)
        await refresh()
        return stored
    }

    func resetBinding(_ id: String) async {
        await send("reset_binding", ["id": .string(id)], key: "binding_\(id)")
        await refresh()
    }

    /// Arm the core's own key listener. The message it returns is the reason
    /// it refused; `nil` means it is listening.
    func startHandyKeysRecording(_ id: String) async -> String? {
        do {
            try await core.request("start_handy_keys_recording", ["bindingId": JSONValue.string(id)])
            error = nil
            return nil
        } catch {
            /// The core refuses while macOS secure input holds the keyboard:
            /// the listener would see the modifiers and never the key.
            let blocked = (error as? CoreError)?.remote(as: String.self) == "secure-input-active"
                || error.localizedDescription.contains("secure-input-active")
            let reason = blocked
                ? "macOS Secure Input is blocking key events, so shortcuts cannot be recorded. Resolve the Secure Input warning first."
                : "Couldn't set the shortcut: \(error.localizedDescription)"
            self.error = reason
            return reason
        }
    }

    func stopHandyKeysRecording() async {
        await send("stop_handy_keys_recording", key: "handy_keys_recording")
    }

    /// Around a capture on the system layer: nothing should fire, or eat the
    /// keys, while they are being read.
    func suspendAllBindings() async {
        await send("suspend_all_bindings", key: "bindings_suspended")
    }

    func resumeAllBindings() async {
        await send("resume_all_bindings", key: "bindings_suspended")
    }
}

import AppKit
import Foundation

/// What build this is, where it came from, and where it keeps things.
///
/// The page leads with three choices that are not build facts — the language
/// Sona speaks, its appearance, and the material its windows are made of —
/// because each is set once per install and this is the least prominent page
/// in Settings. Then the version and the update preference, the source and
/// its licenses, and last the two absolute paths nobody reads on the way past.
@MainActor
@Observable
final class AboutStore {
    /// Printed without its scheme, opened whole.
    static let repositoryURL = "https://github.com/aktanazat/sona"
    static let repositoryLicense = "MIT licensed. Copyright 2025 CJ Pais."

    /// Whatever the update surface currently has to say: one line, at most one
    /// act. A failed save and a failed check cannot both be the most recent
    /// thing that happened.
    struct Status {
        enum Tone { case muted, info, danger }
        enum Act: Equatable {
            case retry
            case viewRelease(String)
        }

        let text: String
        let tone: Tone
        var act: Act?
    }

    /// The running build, read from this bundle. `nil` means the bundle did
    /// not carry a version, which a completed check can still rescue.
    private(set) var version: String?
    private(set) var settings: AboutSettings?
    private(set) var appDirectory: String?
    private(set) var logDirectory: String?
    /// A path that could not be read, named in the row rather than swallowed.
    private(set) var directoryError: String?
    /// True when Sona runs from a portable install and keeps its data beside
    /// the app rather than in the user's Library.
    private(set) var portable = false

    private(set) var checking = false
    private(set) var savingPreference = false
    private(set) var checkResult: UpdateCheckResult?
    private(set) var checkError: String?
    private(set) var preferenceError: String?
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        core.observe(CoreEvent.aboutSettingsChanged) { [weak self] _ in self?.reloadSettings() }
        core.observe(CoreEvent.aboutThemeChanged) { [weak self] _ in self?.reloadSettings() }
    }

    /// The first load: settings, both directories, and whether this is a
    /// portable install.
    func start() async {
        await loadSettings()
        await loadDirectories()
    }

    // MARK: - Reading

    private func loadSettings() async {
        do {
            let settings: AboutSettings = try await core.request("get_app_settings")
            self.settings = settings
            applyAppearance(settings.theme ?? .system)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func reloadSettings() {
        Task { await loadSettings() }
    }

    private func loadDirectories() async {
        do {
            let appDirectory: String = try await core.request("get_app_dir_path")
            let logDirectory: String = try await core.request("get_log_dir_path")
            let portable: Bool = try await core.request("is_portable")
            self.appDirectory = appDirectory
            self.logDirectory = logDirectory
            self.portable = portable
            directoryError = nil
        } catch {
            directoryError = error.localizedDescription
        }
    }

    // MARK: - Version and updates

    /// The version to print: this bundle's, or the one a completed check
    /// reported when the bundle would not answer.
    var displayVersion: String? { version ?? checkResult?.currentVersion }

    var updateCheckEnabled: Bool { settings?.updateCheckEnabled ?? true }
    var showWhatsNewOnUpdate: Bool { settings?.showWhatsNewOnUpdate ?? true }

    /// One status line for the whole update surface.
    var status: Status? {
        if let preferenceError {
            return Status(
                text: "The update preference could not be saved. \(preferenceError)",
                tone: .danger
            )
        }
        if let checkError {
            return Status(
                text: "The update check could not run. \(checkError)",
                tone: .danger,
                act: .retry
            )
        }
        guard let result = checkResult else { return nil }
        switch result.status {
        case .disabled:
            // The switch above is what turns checks back on, so this line only
            // reports that nothing was asked of GitHub.
            return Status(text: "Automatic checks are off, so Sona did not contact GitHub.", tone: .muted)
        case .checkFailed:
            let reason = result.error.map { " \($0)" } ?? ""
            return Status(text: "The update check could not run.\(reason)", tone: .danger, act: .retry)
        case .updateAvailable:
            guard let latest = result.latestVersion else {
                return Status(text: "This is the latest release.", tone: .muted)
            }
            return Status(
                text: "Sona \(latest) is available.",
                tone: .info,
                act: result.url.map { Status.Act.viewRelease($0) }
            )
        case .upToDate:
            return Status(text: "This is the latest release.", tone: .muted)
        }
    }

    /// Asks GitHub whether a newer release exists. The core enforces the
    /// preference: with checks off it reports `disabled` and makes no request,
    /// so this surface never claims a check happened when it did not.
    func checkNow() {
        guard !checking else { return }
        checking = true
        Task {
            do {
                let result: UpdateCheckResult = try await core.request("check_for_updates")
                checkResult = result
                checkError = nil
                error = nil
                if result.status == .disabled, settings != nil {
                    await loadSettings()
                }
            } catch {
                checkResult = nil
                checkError = error.localizedDescription
            }
            checking = false
        }
    }

    func setUpdateCheckEnabled(_ enabled: Bool) {
        savingPreference = true
        Task {
            do {
                try await core.request("change_update_check_enabled_setting", ["enabled": enabled])
                // A stale verdict would contradict the switch beside it.
                checkResult = nil
                checkError = nil
                preferenceError = nil
                error = nil
                await loadSettings()
            } catch {
                preferenceError = error.localizedDescription
            }
            savingPreference = false
        }
    }

    func setShowWhatsNewOnUpdate(_ enabled: Bool) {
        call { try await self.core.request("change_show_whats_new_on_update_setting", ["enabled": enabled]) }
    }

    // MARK: - Appearance

    var language: AppLanguage {
        AppLanguage.supported(settings?.appLanguage) ?? AppLanguage.all[0]
    }

    var theme: AppearanceTheme { settings?.theme ?? .system }
    var material: AppearanceMaterial { settings?.appearanceMaterial ?? .solid }

    func setLanguage(_ code: String) {
        call { try await self.core.request("change_app_language_setting", ["language": code]) }
    }

    func setTheme(_ theme: AppearanceTheme) {
        applyAppearance(theme)
        call { try await self.core.request("change_theme_setting", ["theme": theme.rawValue]) }
    }

    func setMaterial(_ material: AppearanceMaterial) {
        call { try await self.core.request("change_appearance_material_setting", ["material": material.rawValue]) }
    }

    /// The shell's own windows follow the choice immediately; `Theme`'s colours
    /// are resolved against the running appearance, so this is what makes the
    /// row do something the moment it is used.
    private func applyAppearance(_ theme: AppearanceTheme) {
        switch theme {
        case .system: NSApplication.shared.appearance = nil
        case .light: NSApplication.shared.appearance = NSAppearance(named: .aqua)
        case .dark: NSApplication.shared.appearance = NSAppearance(named: .darkAqua)
        }
    }

    // MARK: - Opening things

    func openRepository() {
        guard let url = URL(string: Self.repositoryURL) else { return }
        NSWorkspace.shared.open(url)
    }

    func openLicenseNotices() {
        call { try await self.core.request("open_license_notices") }
    }

    func openAppDataDirectory() {
        call { try await self.core.request("open_app_data_dir") }
    }

    func openLogDirectory() {
        call { try await self.core.request("open_log_dir") }
    }

    func openRecordingsFolder() {
        call { try await self.core.request("open_recordings_folder") }
    }

    // MARK: - Plumbing

    private func call(_ work: @escaping () async throws -> Void) {
        Task {
            do {
                try await work()
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

import Foundation
import Observation

struct MeetingAboutMe: Codable, Equatable {
    var name: String
    var email: String
    var role: String
    var company: String
    var context: String
}

struct MeetingPrepPreferences: Codable, Equatable {
    var aboutMe: MeetingAboutMe
    var emailContextEnabled: Bool
    var mailComposeEnabled: Bool
    var webResearchEnabled: Bool
    var overnightEnabled: Bool
    var upcomingAlertsEnabled: Bool
    var notesReadyEnabled: Bool
    var adHocAlertsEnabled: Bool
    var alertMinutes: Int
    var menuBarEnabled: Bool
}

/// The voiceprint Sona keeps of the user, made from their dictations.
struct UserVoiceprint: Decodable, Equatable {
    let dictationCount: Int
    let speechMs: Int
}

private struct UserVoiceStatus: Decodable {
    let voiceprint: UserVoiceprint?
}

enum MeetingMailState: String, Decodable {
    case off, ready, empty, unavailable
    case notConfigured = "not_configured"
    case permissionDenied = "permission_denied"

    var detail: String {
        switch self {
        case .off: "Email context is off."
        case .ready: "Recent email was read on this Mac only."
        case .empty: "Mail is connected. No recent email with these attendees was found."
        case .notConfigured: "Add an account in Mail, then try again."
        case .permissionDenied: "Allow Sona to control Mail in System Settings → Privacy & Security → Automation."
        case .unavailable: "Mail did not answer. Open Mail, then try again."
        }
    }
}

struct MeetingMailContext: Decodable {
    let state: MeetingMailState
}

struct MeetingBriefSource: Decodable, Identifiable {
    let id: String
    let label: String
    let text: String
    let meetingId: MeetingSessionId?
    let externalUrl: String?
}

struct MeetingBriefPoint: Decodable {
    let text: String
    let sources: [String]
}

struct MeetingBriefContent: Decodable {
    let highlights: [MeetingBriefPoint]
    let agenda: [MeetingBriefPoint]
}

struct MeetingBrief: Decodable {
    let eventKey: String
    let title: String
    let startUtcMs: Int64
    let generatedAtUtcMs: Int64
    let content: MeetingBriefContent
    let sources: [MeetingBriefSource]
    let mail: MeetingMailContext
    let status: String
    let webStatus: String
}

/// The label and its destination are computed together, so a changing clock
/// cannot leave a new meeting label opening the previous event's brief.
struct MeetingMenuSummary {
    let label: String
    let event: UpcomingRow?

    static func make(rows: [UpcomingRow], now: Date) -> MeetingMenuSummary {
        let milliseconds = Int64(now.timeIntervalSince1970 * 1_000)
        let eligible = rows.filter {
            $0.endUtcMs > milliseconds && $0.attendeeCount > 0
                && $0.endUtcMs - $0.startUtcMs <= 6 * 60 * 60_000
        }.sorted { $0.startUtcMs < $1.startUtcMs }
        let next = eligible.first { $0.startUtcMs > milliseconds }
        if let next, next.startUtcMs - milliseconds <= 10 * 60_000 {
            let minutes = max(1, (next.startUtcMs - milliseconds + 59_999) / 60_000)
            return MeetingMenuSummary(label: "\(short(next.title)) · in \(minutes)m", event: next)
        }
        if let current = eligible.last(where: { $0.startUtcMs <= milliseconds }) {
            return MeetingMenuSummary(label: "\(short(current.title)) · now", event: current)
        }
        guard let next else { return MeetingMenuSummary(label: "Free today", event: nil) }
        let minutes = (next.startUtcMs - milliseconds + 59_999) / 60_000
        if minutes <= 60 {
            return MeetingMenuSummary(label: "\(short(next.title)) · in \(minutes)m", event: next)
        }
        let time = next.start.formatted(date: .omitted, time: .shortened)
        let day = Calendar.current.isDateInToday(next.start) ? "" : "tomorrow "
        return MeetingMenuSummary(label: "Free until \(day)\(time)", event: next)
    }

    /// The menu bar has room for about 30 characters of title.
    private static func short(_ title: String) -> String {
        title.count > 30 ? String(title.prefix(29)) + "…" : title
    }
}

@Observable @MainActor
final class MeetingPrepStore {
    private let core: Core
    private(set) var preferences: MeetingPrepPreferences?
    private(set) var preferencesLoading = false
    private(set) var saving = false
    private(set) var settingsError: String?
    private(set) var savedNotice: String?
    private(set) var mailChecking = false
    private(set) var mailStatus: String?
    private(set) var webConnection: SecretState?
    private(set) var webConnectionLoading = false
    private(set) var webBusy = false
    private(set) var webNotice: String?
    private(set) var webError: String?
    private(set) var userVoice: UserVoiceprint?
    private(set) var userVoiceLoaded = false
    private(set) var userVoiceBusy = false
    private(set) var userVoiceError: String?
    private(set) var brief: MeetingBrief?
    private(set) var briefLoading = false
    private(set) var briefError: String?
    private(set) var eventKey: String?
    private(set) var upcoming: UpcomingEvents?
    private(set) var menuLoaded = false
    private(set) var menuError: String?
    /// The minute the menu bar label was last worked out for.
    private(set) var now = Date()
    private var briefRequest = UUID()
    /// The write the next one waits for, so writes land in the order they were
    /// asked for and each About me write builds on what the last one saved.
    @ObservationIgnored private var lastWrite: Task<Bool, Never>?

    init(core: Core) { self.core = core }

    func loadPreferences() async {
        guard !preferencesLoading else { return }
        preferencesLoading = true
        defer { preferencesLoading = false }
        do {
            preferences = try await core.request("meeting_prep_preferences_get")
            settingsError = nil
        } catch {
            settingsError = error.localizedDescription
        }
    }

    /// Saves the meeting settings. The profile has its own page and is
    /// written by `saveAboutMe`, so this keeps whatever profile was saved.
    func save(_ value: MeetingPrepPreferences) async {
        guard !saving else { return }
        saving = true
        savedNotice = nil
        defer { saving = false }
        let saved = await write { current in
            var next = value
            if let current { next.aboutMe = current.aboutMe }
            return next
        }
        if saved {
            savedNotice = "Saved. Your next notes and drafts will use this context."
        }
    }

    /// Saves the profile over the preferences last saved. Answers whether
    /// the saved profile is now `aboutMe`.
    func saveAboutMe(_ aboutMe: MeetingAboutMe) async -> Bool {
        _ = await write { saved in
            guard var next = saved, next.aboutMe != aboutMe else { return nil }
            next.aboutMe = aboutMe
            return next
        }
        return preferences?.aboutMe == aboutMe
    }

    /// Writes what `change` makes of the saved preferences, once every write
    /// asked for earlier has landed. `change` answers nil when there is
    /// nothing to write. Answers whether this write landed.
    private func write(_ change: @escaping @MainActor (MeetingPrepPreferences?) -> MeetingPrepPreferences?) async -> Bool {
        let previous = lastWrite
        let current = Task { @MainActor in
            _ = await previous?.value
            guard let value = change(preferences) else { return false }
            settingsError = nil
            do {
                // The core's preferences are snake_case and refuse unknown keys.
                let encoder = JSONEncoder()
                encoder.keyEncodingStrategy = .convertToSnakeCase
                let payload = try JSONDecoder().decode(JSONValue.self, from: encoder.encode(value))
                preferences = try await core.request("meeting_prep_preferences_set", ["preferences": payload])
                // Invalidate the visible brief too when an email/profile choice changes.
                invalidateBrief()
                mailStatus = nil
                return true
            } catch {
                settingsError = error.localizedDescription
                return false
            }
        }
        lastWrite = current
        guard await current.value else { return false }
        await loadMenu()
        return true
    }

    func checkMail() async {
        guard !mailChecking else { return }
        mailChecking = true
        defer { mailChecking = false }
        do {
            let context: MeetingMailContext = try await core.request("meeting_mail_context_check")
            mailStatus = context.state == .empty ? "Mail is connected. Email is read only on this Mac." : context.state.detail
        } catch {
            mailStatus = error.localizedDescription
        }
    }

    func loadWebConnection() async {
        guard !webConnectionLoading, !webBusy else { return }
        webConnectionLoading = true
        defer { webConnectionLoading = false }
        do {
            webConnection = try await core.request("meeting_web_research_connection")
            webError = nil
        } catch {
            webError = error.localizedDescription
        }
    }

    func saveWebKey(_ token: String) async -> Bool {
        guard !webBusy, !webConnectionLoading else { return false }
        webBusy = true
        webError = nil
        webNotice = nil
        defer { webBusy = false }
        do {
            webConnection = try await core.request(
                "meeting_web_research_key_set", ["token": JSONValue.string(token)])
            invalidateBrief()
            webNotice = "Key saved in Keychain. Test it before enabling web research."
            return true
        } catch {
            webError = error.localizedDescription
            return false
        }
    }

    func removeWebKey() async {
        guard !webBusy, !webConnectionLoading else { return }
        webBusy = true
        webError = nil
        webNotice = nil
        defer { webBusy = false }
        do {
            webConnection = try await core.request("meeting_web_research_key_remove")
            invalidateBrief()
            webNotice = "Key removed. No web searches can run until you save another key."
        } catch {
            webError = error.localizedDescription
        }
    }

    func testWebConnection() async {
        guard !webBusy, !webConnectionLoading else { return }
        webBusy = true
        webError = nil
        webNotice = nil
        defer { webBusy = false }
        do {
            webNotice = try await core.request("meeting_web_research_test")
        } catch {
            webError = error.localizedDescription
        }
    }

    func loadUserVoice() async {
        guard !userVoiceBusy else { return }
        do {
            let status: UserVoiceStatus = try await core.request("voice_user_status")
            userVoice = status.voiceprint
            userVoiceLoaded = true
            userVoiceError = nil
        } catch {
            userVoiceError = error.localizedDescription
        }
    }

    /// Learns the user's voice from their recent dictations. The caller has
    /// shown the consent text this version stands for.
    func learnUserVoice() async {
        guard !userVoiceBusy else { return }
        userVoiceBusy = true
        userVoiceError = nil
        defer { userVoiceBusy = false }
        do {
            let status: UserVoiceStatus = try await core.request(
                "voice_user_learn", ["consentVersion": JSONValue.number(1)])
            userVoice = status.voiceprint
        } catch {
            userVoiceError = switch (error as? CoreError)?.remote(as: MeetingCommandError.self) {
            case .insufficientEnrollmentEvidence:
                "Sona needs about 20 seconds of your recent dictation first. Dictate a little more, then try again."
            case .localModelUnavailable:
                "Sona could not get its voice model. Check your internet connection, then try again."
            case let failure?: failure.label
            case nil: error.localizedDescription
            }
        }
    }

    func forgetUserVoice() async {
        guard !userVoiceBusy else { return }
        userVoiceBusy = true
        userVoiceError = nil
        defer { userVoiceBusy = false }
        do {
            let status: UserVoiceStatus = try await core.request("voice_user_forget")
            userVoice = status.voiceprint
        } catch {
            userVoiceError = error.localizedDescription
        }
    }

    private func invalidateBrief() {
        briefRequest = UUID()
        brief = nil
        briefLoading = false
    }

    func openBrief(_ key: String) {
        eventKey = key
        brief = nil
        Task { await loadBrief(refresh: false) }
    }

    func loadBrief(refresh: Bool) async {
        guard let eventKey else { return }
        let request = UUID()
        briefRequest = request
        briefLoading = true
        briefError = nil
        do {
            let params: [String: JSONValue] = ["eventKey": .string(eventKey), "refresh": .bool(refresh)]
            let value: MeetingBrief = try await core.request("meeting_brief_get", params)
            guard briefRequest == request else { return }
            brief = value
        } catch {
            guard briefRequest == request else { return }
            briefError = error.localizedDescription
        }
        if briefRequest == request { briefLoading = false }
    }

    /// The prep card's short read of a brief. Kept apart from the open brief
    /// so the card and the sheet never overwrite each other.
    func preview(_ eventKey: String) async throws -> MeetingBrief {
        let params: [String: JSONValue] = ["eventKey": .string(eventKey), "refresh": .bool(false)]
        return try await core.request("meeting_brief_get", params)
    }

    func watchMenu() async {
        while !Task.isCancelled {
            now = .now
            await loadPreferences()
            await loadMenu()
            // Wake at the next minute, so "in 5m" becomes "in 4m" on time.
            let wait = 60 - Double(Calendar.current.component(.second, from: .now))
            do { try await Task.sleep(for: .seconds(wait)) } catch { return }
        }
    }

    private func loadMenu() async {
        guard preferences?.menuBarEnabled == true else {
            upcoming = nil
            return
        }
        do {
            upcoming = try await core.request("meeting_upcoming_events", ["days": JSONValue.number(1)])
            menuError = nil
        } catch {
            upcoming = nil
            menuError = error.localizedDescription
        }
        menuLoaded = true
    }

    var menuSummary: MeetingMenuSummary {
        if let menuError { return MeetingMenuSummary(label: "Calendar unavailable: \(menuError)", event: nil) }
        guard menuLoaded, let upcoming else { return MeetingMenuSummary(label: "Reading calendar…", event: nil) }
        guard upcoming.access == .authorized else {
            return MeetingMenuSummary(label: "Enable Calendar in Meeting settings", event: nil)
        }
        return MeetingMenuSummary.make(rows: upcoming.rows, now: now)
    }
}

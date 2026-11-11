import Foundation
import SwiftUI

/// Shared meeting pages: the shapes of the core's `shared_*` commands.
///
/// Replies decode through `Core.decoder`, which turns snake_case into the
/// camelCase names here. What a request carries is spelled with the core's
/// own names in `encode(to:)`, because the request encoder converts nothing.

/// Which parts of the meeting go on the page. Toggling one never touches
/// the text it filters.
struct SharedInclude: Codable, Equatable {
    var title: Bool
    var summary: Bool
    var actionItems: Bool
    var decisions: Bool
    var notes: Bool
    var transcript: Bool
    var audio: Bool

    private enum WireKey: String, CodingKey {
        case title, summary, decisions, notes, transcript, audio
        case actionItems = "action_items"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encode(title, forKey: .title)
        try container.encode(summary, forKey: .summary)
        try container.encode(actionItems, forKey: .actionItems)
        try container.encode(decisions, forKey: .decisions)
        try container.encode(notes, forKey: .notes)
        try container.encode(transcript, forKey: .transcript)
        try container.encode(audio, forKey: .audio)
    }
}

/// How the transcript names the people in it.
enum SharedSpeakers: String, Codable, CaseIterable {
    case names
    case numbered

    var label: String {
        switch self {
        case .names: "Names"
        case .numbered: "Speaker 1, 2"
        }
    }
}

/// How long a link stays up. The core takes days or nothing.
enum SharedExpiry: Int, CaseIterable {
    case never = 0
    case day = 1
    case week = 7
    case month = 30

    init(days: Int?) {
        self = days.flatMap(SharedExpiry.init(rawValue:)) ?? .never
    }

    var days: Int? { self == .never ? nil : rawValue }

    var label: String {
        switch self {
        case .never: "Never"
        case .day: "1 day"
        case .week: "7 days"
        case .month: "30 days"
        }
    }
}

struct SharedActionItem: Codable, Equatable {
    var text: String
    var owner: String?
    var due: String?
}

/// One speaker turn: `s` indexes the content's speakers, `t` is seconds
/// from the start of the recording.
struct SharedTurn: Codable, Equatable {
    var s: Int
    var t: Double
    var text: String
}

/// Everything the meeting has, with his edits. `include` and `speakers`
/// filter it only when the page is made.
struct SharedContent: Codable {
    var title: String?
    var date: String?
    var durationS: Double?
    var summary: [String]
    var decisions: [String]
    var actionItems: [SharedActionItem]
    var notes: [String]
    var speakers: [String]
    var transcript: [SharedTurn]

    private enum ReadKey: String, CodingKey {
        case title, date, durationS, summary, decisions, actionItems, notes, speakers, transcript
    }

    private enum WireKey: String, CodingKey {
        case title, date, summary, decisions, notes, speakers, transcript
        case durationS = "duration_s"
        case actionItems = "action_items"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: ReadKey.self)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        date = try container.decodeIfPresent(String.self, forKey: .date)
        durationS = try container.decodeIfPresent(Double.self, forKey: .durationS)
        summary = try container.decodeIfPresent([String].self, forKey: .summary) ?? []
        decisions = try container.decodeIfPresent([String].self, forKey: .decisions) ?? []
        actionItems = try container.decodeIfPresent([SharedActionItem].self, forKey: .actionItems) ?? []
        notes = try container.decodeIfPresent([String].self, forKey: .notes) ?? []
        speakers = try container.decodeIfPresent([String].self, forKey: .speakers) ?? []
        transcript = try container.decodeIfPresent([SharedTurn].self, forKey: .transcript) ?? []
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encode(title, forKey: .title)
        try container.encode(date, forKey: .date)
        try container.encode(durationS, forKey: .durationS)
        try container.encode(summary, forKey: .summary)
        try container.encode(decisions, forKey: .decisions)
        try container.encode(actionItems, forKey: .actionItems)
        try container.encode(notes, forKey: .notes)
        try container.encode(speakers, forKey: .speakers)
        try container.encode(transcript, forKey: .transcript)
    }
}

/// `draft.json`: one meeting's page before it is shared.
struct SharedDraft: Codable {
    var version: Int
    var meetingId: String
    var preparedBy: String?
    var include: SharedInclude
    var speakers: SharedSpeakers
    var expiresInDays: Int?
    /// A version of the page written for one person: the action items keep
    /// only his. `nil` shares the whole meeting.
    var forPerson: String?
    var content: SharedContent
    var audioAvailable: Bool

    private enum WireKey: String, CodingKey {
        case version, include, speakers, content
        case meetingId = "meeting_id"
        case preparedBy = "prepared_by"
        case expiresInDays = "expires_in_days"
        case forPerson = "for_person"
        case audioAvailable = "audio_available"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encode(version, forKey: .version)
        try container.encode(meetingId, forKey: .meetingId)
        try container.encode(preparedBy, forKey: .preparedBy)
        try container.encode(include, forKey: .include)
        try container.encode(speakers, forKey: .speakers)
        try container.encode(expiresInDays, forKey: .expiresInDays)
        try container.encode(forPerson, forKey: .forPerson)
        try container.encode(content, forKey: .content)
        try container.encode(audioAvailable, forKey: .audioAvailable)
    }
}

/// `shared_preview`: the exact page, and the files that draw it here.
struct SharedPreview: Decodable {
    let page: JSONValue
    let htmlPath: String
    let audioPath: String?
    /// What the check found on the page that may not belong in a link.
    let checks: [ShareCheck]
}

/// One thing the preview check flagged: `text` is the exact substring on
/// the page, `index` places the flag on screen.
struct ShareCheck: Decodable {
    let kind: String
    let section: String
    let index: Int
    let text: String

    /// "Phone number 555-0142 in the summary"; a name from his people who
    /// was not in the meeting says so.
    var words: String {
        kind == "person"
            ? "\(text) is named in the \(place) but isn't a named speaker in this meeting"
            : "\(noun) \(text) in the \(place)"
    }

    private var noun: String {
        switch kind {
        case "phone": "Phone number"
        case "email": "Email address"
        case "money": "Amount"
        case "link": "Link"
        case "number": "Number"
        case "person": "Name"
        default: kind.capitalized
        }
    }

    private var place: String {
        switch section {
        case "title": "title"
        case "for_person": "for-person line"
        case "summary": "summary"
        case "decisions": "decisions"
        case "action_items": "action items"
        case "notes": "notes"
        case "speakers": "speakers"
        case "transcript": "transcript"
        default: section
        }
    }
}

/// Who the page is written for: the whole meeting, or only the action
/// items owned by one person.
enum ShareForChoice: Hashable {
    case everyone
    case person(String)

    var label: String {
        switch self {
        case .everyone: "Everyone"
        case let .person(name): name
        }
    }
}

/// `shared_draft_recap`: the meeting rewritten for someone who wasn't
/// there, as a few short paragraphs.
struct ShareRecap: Decodable {
    let paragraphs: [String]
}

/// A draft waiting for him in the Shared list.
struct SharedDraftSummary: Decodable, Identifiable {
    let meetingId: String
    let title: String?
    let preparedBy: String?
    let savedAt: String?

    var id: String { meetingId }
}

/// Where pages go: the private repository and the address its Pages site
/// serves.
struct SharedSite: Codable, Equatable {
    let remote: String
    let baseUrl: String

    private enum WireKey: String, CodingKey {
        case remote
        case baseUrl = "base_url"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encode(remote, forKey: .remote)
        try container.encode(baseUrl, forKey: .baseUrl)
    }

    /// "aktan.us/m": the address without its scheme.
    var place: String {
        baseUrl.replacingOccurrences(of: "https://", with: "").replacingOccurrences(of: "http://", with: "")
    }
}

/// Where one link stands. Expired is read from the date, never stored.
enum SharedState: Equatable {
    case goingLive
    case live
    case removing
    case removed
    case expired
    case failed
    case other(String)

    init(raw: String, expiresAt: Date?) {
        switch raw {
        case "publishing": self = .goingLive
        case "live": self = expiresAt.map { $0 <= .now } == true ? .expired : .live
        case "removing": self = .removing
        case "removed": self = .removed
        case "expired": self = .expired
        case "failed": self = .failed
        default: self = .other(raw)
        }
    }

    var word: String {
        switch self {
        case .goingLive: "Going live"
        case .live: "Live"
        case .removing: "Removing"
        case .removed: "Removed"
        case .expired: "Expired"
        case .failed: "Failed"
        case let .other(raw): raw.capitalized
        }
    }

    var tone: Color {
        switch self {
        case .live: Theme.ink
        case .goingLive, .removing: Theme.accent
        case .failed: Theme.live
        case .removed, .expired, .other: Theme.inkTertiary
        }
    }

    /// A state the core is still settling; the list reads again until it
    /// has.
    var settling: Bool { self == .goingLive || self == .removing }

    /// A page that may be online, so it can be taken down.
    var stoppable: Bool { self == .live || self == .goingLive }
}

/// One shared link: `shares.json`'s record.
struct SharedRecord: Decodable, Identifiable {
    let slug: String
    let url: String?
    let meetingId: String?
    let title: String?
    /// Set when the page was written for one person; the list says
    /// "for <name>".
    let forPerson: String?
    /// The first summary sentence, kept beside the link so it can be
    /// pasted into a chat with it.
    let blurb: String?
    let include: [String]
    let speakers: SharedSpeakers?
    let createdAt: String
    let expiresAt: String?
    let state: String
    let stateAt: String?
    let bytes: Int?
    let error: String?
    var id: String { slug }

    var expiry: Date? { SharedClock.date(expiresAt) }
    var created: Date? { SharedClock.date(createdAt) }
    var phase: SharedState { SharedState(raw: state, expiresAt: expiry) }

    /// What the page carries, in words: "Summary, action items and
    /// transcript, speakers numbered".
    var contents: String {
        let words: [(String, String)] = [
            ("summary", "summary"), ("action_items", "action items"), ("decisions", "decisions"),
            ("notes", "notes"), ("transcript", "transcript"), ("audio", "audio"),
        ]
        let parts = words.filter { include.contains($0.0) }.map(\.1)
        guard var sentence = SharedRecord.list(parts) else {
            return include.contains("title") ? "Title and date only" : "An empty page"
        }
        if include.contains("transcript"), speakers == .numbered {
            sentence += ", speakers numbered"
        }
        return sentence
    }

    /// "Shared 10 Nov · Expires 17 Nov".
    var dates: String {
        var parts: [String] = []
        if let created { parts.append("Shared \(created.short)") }
        if let expiry {
            parts.append(expiry <= .now ? "Expired \(expiry.short)" : "Expires \(expiry.short)")
        } else {
            parts.append("No expiry")
        }
        return parts.joined(separator: " · ")
    }

    private static func list(_ parts: [String]) -> String? {
        guard let first = parts.first else { return nil }
        let head = first.prefix(1).uppercased() + first.dropFirst()
        let all = [head] + parts.dropFirst()
        if all.count == 1 { return all[0] }
        return all.dropLast().joined(separator: ", ") + " and " + all[all.count - 1]
    }
}

/// The core's RFC 3339 times.
enum SharedClock {
    private static let fractional = Date.ISO8601FormatStyle(includingFractionalSeconds: true)

    static func date(_ text: String?) -> Date? {
        guard let text, !text.isEmpty else { return nil }
        return (try? Date(text, strategy: .iso8601)) ?? (try? Date(text, strategy: fractional))
    }
}

/// The core's refusals, in plain words. A refusal arrives as its code, as
/// "code: detail", or as an object naming both.
enum SharedFailure {
    static func words(_ failure: Error) -> String {
        guard let core = failure as? CoreError, case let .remote(value) = core else {
            return failure.localizedDescription
        }
        let (code, detail) = parts(value)
        switch code {
        case "site_not_set_up":
            return "Add where pages go first: the repository and the address."
        case "meeting_not_found":
            return "This meeting is no longer on this Mac."
        case "too_large":
            return "This page is too large to share. Leave out the audio or the transcript and try again."
        case "audio_unavailable":
            return "The recording for this meeting is not on this Mac. Share it without audio."
        case "push_failed":
            let reason = detail.map { " GitHub said: \($0)" } ?? ""
            return "GitHub did not take the page, so nothing went live.\(reason)"
        case "not_found":
            return "That link is no longer in the list."
        case "locked":
            return "Another share is being written right now. Try again in a moment."
        case "remote_not_sona_site":
            return "That repository already holds something else. Use an empty repository for shared pages."
        case "invalid_draft":
            return "Part of this draft is not in a form Sona can read. Discard it and start again from the meeting."
        case "no_items_for_person":
            return "A page for one person needs at least one of their action items. Turn on action items, pick someone else under For, or choose Everyone."
        case "failed":
            return "Sona could not finish that. Try again."
        case "unavailable":
            return "Meetings aren't open yet. Try again in a moment."
        case "no_store":
            return "The meeting's records aren't open, so it can't be rewritten right now."
        case "no_engine":
            return "No way to rewrite text is set up. Check Settings and try again."
        case "recording":
            return "The meeting is still recording. Stop it first, then try again."
        default:
            return detail ?? code ?? value.message
        }
    }

    static func code(_ failure: Error) -> String? {
        guard let core = failure as? CoreError, case let .remote(value) = core else { return nil }
        return parts(value).code
    }

    private static func parts(_ value: JSONValue) -> (code: String?, detail: String?) {
        switch value {
        case let .string(text):
            if let colon = text.firstIndex(of: ":") {
                let code = String(text[..<colon])
                if code.allSatisfy({ $0.isLowercase || $0 == "_" }) {
                    let detail = text[text.index(after: colon)...].trimmingCharacters(in: .whitespaces)
                    return (code, detail.isEmpty ? nil : detail)
                }
            }
            return text.allSatisfy({ $0.isLowercase || $0 == "_" }) ? (text, nil) : (nil, text)
        case let .object(object):
            let code = ["error", "code", "kind"].lazy.compactMap { object[$0]?.text }.first
            let detail = ["message", "detail", "reason"].lazy.compactMap { object[$0]?.text }.first
            return (code, detail)
        default:
            return (nil, nil)
        }
    }
}

private extension JSONValue {
    var text: String? {
        if case let .string(text) = self { text } else { nil }
    }
}

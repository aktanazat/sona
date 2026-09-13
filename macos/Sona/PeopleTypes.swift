import Foundation

/// The people corpus as the core sends it: `src-tauri/src/meeting/people_types.rs`
/// and `src-tauri/src/commands/voice_identity.rs`, with snake_case turned into
/// camelCase by `Core.decoder`. Enum values are the wire strings verbatim.
///
/// Requests are hand written with snake_case coding keys: the dispatcher takes
/// the argument by its camelCase name (`personId`, `request`), but the value
/// inside `request` is a plain serde struct, so its fields keep the Rust
/// spelling. They are structs rather than dictionaries because every one of
/// them carries a revision, and a revision has to reach serde as an integer.

extension CoreEvent {
    /// A meeting's artifacts changed: its ledger is what a person's open loops
    /// and commitments are read out of, so the people pages re-read on it.
    static let peopleArtifactChanged = "meeting:artifact-changed"
    /// A meeting was deleted, which takes its links with it.
    static let peopleMeetingRemoved = "meeting:removed"
}

// MARK: - Errors

/// The core's own refusal for every people command (`MeetingCommandError`).
/// The shell reacts to `staleRevision` the way the web app did — say the
/// corpus moved, then re-read — and reports the rest as the sentence the
/// command earned.
enum PeopleCommandError: String, Decodable {
    case consentRequired = "consent_required"
    case consentStale = "consent_stale"
    case invalidTransition = "invalid_transition"
    case staleRevision = "stale_revision"
    case captureLeaseBusy = "capture_lease_busy"
    case noSourceStarted = "no_source_started"
    case sourceUnavailable = "source_unavailable"
    case storageUnavailable = "storage_unavailable"
    case recoveryRequired = "recovery_required"
    case deletionInProgress = "deletion_in_progress"
    case notFound = "not_found"
    case invalidRequest = "invalid_request"
    case exportCancelled = "export_cancelled"
    case exportFailed = "export_failed"
    case localModelUnavailable = "local_model_unavailable"
    case localEvidenceUnavailable = "local_evidence_unavailable"
    case insufficientEnrollmentEvidence = "insufficient_enrollment_evidence"
    case profileModelIncompatible = "profile_model_incompatible"
    case profileMergeResolutionRequired = "profile_merge_resolution_required"
    case remoteUnavailable = "remote_unavailable"
    case engineFailure = "engine_failure"
    case importUnreadable = "import_unreadable"

    /// What a reader can act on. The three refusals that mean "this Mac kept
    /// no usable sample of that voice" share one sentence, because they share
    /// one answer.
    var message: String {
        switch self {
        case .staleRevision: "People changed on another screen. Reload and try again."
        case .notFound: "This person is no longer in Sona."
        case .storageUnavailable: "Encrypted meeting storage is unavailable."
        case .deletionInProgress: "A deletion is already in progress."
        case .recoveryRequired: "This meeting needs recovery before that change."
        case .invalidRequest: "Sona refused that change as invalid."
        case .localModelUnavailable: "No local model is installed to write that."
        case .engineFailure: "The engine could not finish that."
        case .localEvidenceUnavailable, .insufficientEnrollmentEvidence, .profileModelIncompatible:
            "The label was saved. Sona could not remember this voice from this meeting. Try another meeting."
        case .profileMergeResolutionRequired:
            "Sona needs to know which saved voice to keep before merging these people."
        case .consentRequired: "Confirm the recording acknowledgement first."
        case .consentStale: "The setup changed. Review it again first."
        case .invalidTransition: "That is unavailable in the meeting's current phase."
        case .captureLeaseBusy: "Another meeting is already capturing audio."
        case .noSourceStarted: "No approved audio source started."
        case .sourceUnavailable: "A requested audio source is unavailable."
        case .exportCancelled: "The export was cancelled."
        case .exportFailed: "The export failed."
        case .remoteUnavailable: "The selected remote destination is unavailable."
        case .importUnreadable: "Sona could not read that file."
        }
    }
}

// MARK: - The corpus

/// Where a link came from, weakest last: an invite names a person outright, a
/// voice match is recognition, a title is a guess, and a manual link is you.
enum PersonLinkSource: String, Decodable, CaseIterable {
    case calendar
    case speaker
    case title
    case manual

    var label: String {
        switch self {
        case .calendar: "Calendar"
        case .speaker: "Speaker"
        case .title: "Title"
        case .manual: "Manual"
        }
    }
}

enum PersonLinkConfidence: String, Decodable {
    case confirmed
    case suggested
}

/// Where a ledger line stands now, read live from its loop state row.
enum PersonLoopStatus: String, Decodable {
    case open
    case done
    case dropped
    case carried

    /// The word worth a chip. "Open" under a heading that reads "Open loops"
    /// is the heading said twice, so only a status that contradicts its
    /// section earns one.
    var contradiction: String? {
        switch self {
        case .open: nil
        case .done: "Done"
        case .dropped: "Dropped"
        case .carried: "Carried"
        }
    }
}

/// Which side of the conversation a line is on. The store decides it once,
/// from the owner the user picked or the voice that said it.
enum PersonLoopDirection: String, Decodable {
    case mine
    case waitingOn = "waiting_on"
    case unattributed
}

/// The relationship paragraph and the two facts that make it readable.
struct PersonSummary: Decodable {
    let text: String
    let generatedAtUtcMs: Int64
    /// The engine that wrote it.
    let modelId: String
}

struct Person: Decodable, Identifiable {
    let id: String
    let displayName: String
    let aliases: [String]
    let calendarEmails: [String]
    let organization: String?
    /// Absent until an artifact pass has had an engine to write it with.
    let summary: PersonSummary?
    let createdAtUtcMs: Int64
    let updatedAtUtcMs: Int64
}

struct PersonMeetingSummary: Decodable, Identifiable {
    let id: String
    let title: String
    let atUtcMs: Int64
    let headline: String?
    let seriesNumber: UInt64

    var at: Date { PeopleFormat.date(atUtcMs) }
}

struct PersonMeetingLink: Decodable, Identifiable {
    let meeting: PersonMeetingSummary
    let source: PersonLinkSource
    let confidence: PersonLinkConfidence

    var id: String { meeting.id }
    var isSuggested: Bool { confidence == .suggested }
}

/// A loop raised in a meeting with this person, as the people surfaces read it.
struct PersonOpenLoop: Decodable, Identifiable {
    let loopId: String
    let meetingId: String
    let title: String
    let atUtcMs: Int64
    let text: String
    let ownerPersonId: String?
    let status: PersonLoopStatus
    let direction: PersonLoopDirection
    /// This person has owed it for longer than a working week.
    let waitingOnStale: Bool
    /// When it was first raised, if it reached this meeting by being carried.
    let carriedSinceAtUtcMs: Int64?
    let carriedIntoMeetingId: String?

    var id: String { loopId }
    var at: Date { PeopleFormat.date(atUtcMs) }
}

struct PersonCommitment: Decodable, Identifiable {
    let loopId: String
    let meetingId: String
    let title: String
    let atUtcMs: Int64
    let text: String
    let status: PersonLoopStatus
    let direction: PersonLoopDirection
    let waitingOnStale: Bool
    let resolvedAtUtcMs: Int64?

    var id: String { loopId }
    var at: Date { PeopleFormat.date(atUtcMs) }
}

/// `{"kind": "ledger", "text": "..."}` or the `summary` twin. Which register
/// the headline came out of does not change how it reads, so the shell keeps
/// the words and drops the register.
struct PersonMeetingHeadline: Decodable {
    let kind: String
    let text: String
}

struct PersonListLastMeeting: Decodable {
    let sessionId: String
    let title: String
    let atMs: Int64
    let headline: PersonMeetingHeadline?

    var at: Date { PeopleFormat.date(atMs) }
}

struct PersonListEntry: Decodable, Identifiable {
    let person: Person
    let meetingsCount: UInt64
    let lastMeetingAtUtcMs: Int64?
    let suggestedCount: UInt64
    let evidenceSources: [PersonLinkSource]
    let confirmedCount: UInt64
    let lastMeeting: PersonListLastMeeting?

    var id: String { person.id }
}

/// One imported document as `person_detail` reports it. The contents and the
/// two verbs that write them belong to the import surface; a person's page
/// shows the catalogue.
struct PersonDocumentSummary: Decodable, Identifiable {
    let id: String
    let title: String
    let sourceName: String
    let mediaType: String
    let createdAtUtcMs: Int64

    var createdAt: Date { PeopleFormat.date(createdAtUtcMs) }
}

struct PersonDetail: Decodable {
    let person: Person
    let links: [PersonMeetingLink]
    let openLoops: [PersonOpenLoop]
    let commitments: [PersonCommitment]
    /// Average share of the talking, in thousandths.
    let talkShareAvgPermille: UInt32?
    let documents: [PersonDocumentSummary]
}

struct PersonDetailResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let detail: PersonDetail
}

struct PeopleListResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let entries: [PersonListEntry]
}

/// What a write answered with: the new revision, the person as it now stands,
/// and whether that person is gone.
struct PeopleMutationResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let person: Person?
    let removed: Bool
}

struct OrganizationDetail: Decodable {
    /// The label as its people carry it, not the slug it was looked up by.
    let name: String
    let people: [PersonListEntry]
    /// Meetings with anybody here, newest first, deduplicated across people.
    let recentMeetings: [PersonMeetingSummary]
    /// What is still open with anybody here, newest first.
    let openLoops: [PersonOpenLoop]
}

struct OrganizationDetailResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let detail: OrganizationDetail
}

/// Everything still open across the whole corpus, newest first.
struct PeopleOpenLoopsInbox: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let entries: [PersonOpenLoop]
}

/// A term Sona keeps hearing in meetings and does not know how to write.
struct PeopleVocabularyCandidate: Decodable, Identifiable {
    let text: String
    let occurrences: UInt64
    let meetingsCount: UInt64

    var id: String { text }
}

struct PeopleVocabularyCandidates: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let entries: [PeopleVocabularyCandidate]
}

/// An answer to one mined term. The store keeps it, not the page: the same
/// decision silences the candidate on every surface that lists it, and the
/// candidate list already arrives with answered terms taken out.
///
/// The key is normalized where it lands and cannot be shown back to anybody,
/// so the display form travels beside it.
struct PeopleLearningDecision: Encodable {
    let candidateKey: String
    let status: String

    private enum CodingKeys: String, CodingKey {
        case loopKind = "loop_kind"
        case candidateKey = "candidate_key"
        case status
        case displayText = "display_text"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode("vocabulary_term", forKey: .loopKind)
        try container.encode(candidateKey, forKey: .candidateKey)
        try container.encode(status, forKey: .status)
        try container.encode(candidateKey, forKey: .displayText)
    }
}

// MARK: - Briefing

struct BriefingLastMeeting: Decodable {
    let id: String
    let title: String
    let atUtcMs: Int64
    let headline: String?

    var at: Date { PeopleFormat.date(atUtcMs) }
}

/// What to walk into a room knowing: how often you have met, when that last
/// was, and what is still open in both directions.
struct BriefingRow: Decodable, Identifiable {
    let personId: String
    let displayName: String
    let meetingsCount: UInt64
    let last: BriefingLastMeeting?
    let openLoops: [PersonOpenLoop]
    let commitments: [PersonCommitment]

    var id: String { personId }
}

struct BriefingResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let rows: [BriefingRow]
}

/// One person already in a meeting's roster, read against everything before it.
struct PersonMeetingContextRow: Decodable, Identifiable {
    let personId: String
    let displayName: String
    let evidenceSource: PersonLinkSource
    let meetingsTogether: UInt64
    let lastPriorMeeting: BriefingLastMeeting?
    let topOpenLoop: PersonOpenLoop?

    var id: String { personId }
}

struct PersonMeetingContextResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let rows: [PersonMeetingContextRow]
}

// MARK: - Voice identity

struct VoiceIdentityStatus: Decodable {
    let unresolvedActiveSpeakerIds: [String]
}

/// One diarized speaker in a meeting, as `meeting_get` reports it. The
/// enrollment request needs this speaker's own revision, which is why the
/// voice flow reads the meeting rather than taking a caller's word for it.
struct VoiceIdentitySpeaker: Decodable, Identifiable {
    let speakerId: String
    let displayName: String
    let revision: UInt64

    var id: String { speakerId }
}

struct VoiceIdentitySession: Decodable {
    let sessionId: String
    let revision: UInt64
}

/// Only the two parts of a meeting the voice flow reads.
struct VoiceIdentityMeeting: Decodable {
    let session: VoiceIdentitySession
    let speakers: [VoiceIdentitySpeaker]
}

/// `voice_identify_speaker` also answers with the operation receipt; the shell
/// acts on the person the label resolved to.
struct VoiceIdentityResult: Decodable {
    let resolvedPersonId: String?
}

struct VoiceProfileEnrollmentStatus: Decodable {
    let enrolled: Bool
    let sampleCount: UInt64
}

// MARK: - Requests

struct PersonRenameRequest: Encodable {
    let personId: String
    let displayName: String
    let expectedRevision: UInt64

    enum CodingKeys: String, CodingKey {
        case personId = "person_id"
        case displayName = "display_name"
        case expectedRevision = "expected_revision"
    }
}

/// Which saved voice survives a merge.
enum PersonVoiceResolution: String, Encodable {
    case discardSource = "discard_source"
    case replaceTargetWithSource = "replace_target_with_source"
    case combineCompatible = "combine_compatible"
}

struct PersonMergeRequest: Encodable {
    let sourcePersonId: String
    let targetPersonId: String
    let expectedRevision: UInt64
    /// Combining keeps both records' samples, which is what merging two
    /// records of one person means. The other two throw one side away, so
    /// neither belongs in a default nobody chose.
    let voiceProfileResolution: PersonVoiceResolution

    enum CodingKeys: String, CodingKey {
        case sourcePersonId = "source_person_id"
        case targetPersonId = "target_person_id"
        case expectedRevision = "expected_revision"
        case voiceProfileResolution = "voice_profile_resolution"
    }
}

struct PersonDeleteRequest: Encodable {
    let personId: String
    let expectedRevision: UInt64

    enum CodingKeys: String, CodingKey {
        case personId = "person_id"
        case expectedRevision = "expected_revision"
    }
}

/// `{"kind": "create", "display_name": "..."}` or the `existing` twin.
enum PersonSplitTarget: Encodable {
    case create(displayName: String)
    case existing(personId: String)

    private enum CodingKeys: String, CodingKey {
        case kind
        case displayName = "display_name"
        case personId = "person_id"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .create(displayName):
            try container.encode("create", forKey: .kind)
            try container.encode(displayName, forKey: .displayName)
        case let .existing(personId):
            try container.encode("existing", forKey: .kind)
            try container.encode(personId, forKey: .personId)
        }
    }
}

struct PersonSplitRequest: Encodable {
    let sourcePersonId: String
    let target: PersonSplitTarget
    let meetingIds: [String]
    let aliases: [String]
    let calendarEmails: [String]
    let documentIds: [String]
    let expectedRevision: UInt64

    enum CodingKeys: String, CodingKey {
        case sourcePersonId = "source_person_id"
        case target
        case meetingIds = "meeting_ids"
        case aliases
        case calendarEmails = "calendar_emails"
        case documentIds = "document_ids"
        case expectedRevision = "expected_revision"
    }
}

/// One link between a meeting and a person, for all three verbs that write it.
struct LinkRequest: Encodable {
    let meetingId: String
    let personId: String
    let expectedRevision: UInt64

    enum CodingKeys: String, CodingKey {
        case meetingId = "meeting_id"
        case personId = "person_id"
        case expectedRevision = "expected_revision"
    }
}

/// A meeting offered for a manual link: only what the picker draws.
struct LinkCandidate: Decodable, Identifiable {
    let sessionId: String
    let title: String
    let createdAtUtcMs: Int64

    var id: String { sessionId }
    var createdAt: Date { PeopleFormat.date(createdAtUtcMs) }
}

struct LinkCandidatePage: Decodable {
    let entries: [LinkCandidate]
    let hasMore: Bool
}

enum VoiceIdentityTarget: Encodable {
    case existing(personId: String)
    case create(displayName: String)

    private enum CodingKeys: String, CodingKey {
        case kind
        case personId = "person_id"
        case displayName = "display_name"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .existing(personId):
            try container.encode("existing", forKey: .kind)
            try container.encode(personId, forKey: .personId)
        case let .create(displayName):
            try container.encode("create", forKey: .kind)
            try container.encode(displayName, forKey: .displayName)
        }
    }
}

/// What to do with one speaker: name them, correct the name, or take the name
/// off and forget the voice.
enum VoiceIdentityAction: Encodable {
    case label(VoiceIdentityTarget)
    case correctTo(VoiceIdentityTarget)
    case markUnknown

    private enum CodingKeys: String, CodingKey {
        case kind
        case target
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .label(target):
            try container.encode("label", forKey: .kind)
            try container.encode(target, forKey: .target)
        case let .correctTo(target):
            try container.encode("correct_to", forKey: .kind)
            try container.encode(target, forKey: .target)
        case .markUnknown:
            try container.encode("mark_unknown", forKey: .kind)
        }
    }
}

struct VoiceIdentityRequest: Encodable {
    let operationId: String
    let requestedAtUtcMs: Int64
    let sessionId: String
    let expectedMeetingRevision: UInt64
    let expectedPeopleRevision: UInt64
    let speakerId: String
    let action: VoiceIdentityAction

    enum CodingKeys: String, CodingKey {
        case operationId = "operation_id"
        case requestedAtUtcMs = "requested_at_utc_ms"
        case sessionId = "session_id"
        case expectedMeetingRevision = "expected_meeting_revision"
        case expectedPeopleRevision = "expected_people_revision"
        case speakerId = "speaker_id"
        case action
    }
}

struct VoiceProfileEnrollmentRequest: Encodable {
    let personId: String
    let sessionId: String
    let speakerId: String
    let expectedMeetingRevision: UInt64
    let expectedSpeakerRevision: UInt64
    let expectedPeopleRevision: UInt64
    let consentVersion: UInt32

    enum CodingKeys: String, CodingKey {
        case personId = "person_id"
        case sessionId = "session_id"
        case speakerId = "speaker_id"
        case expectedMeetingRevision = "expected_meeting_revision"
        case expectedSpeakerRevision = "expected_speaker_revision"
        case expectedPeopleRevision = "expected_people_revision"
        case consentVersion = "consent_version"
    }
}

struct VoiceProfileRemovalRequest: Encodable {
    let personId: String
    let expectedPeopleRevision: UInt64

    enum CodingKeys: String, CodingKey {
        case personId = "person_id"
        case expectedPeopleRevision = "expected_people_revision"
    }
}

// MARK: - Reading the corpus

/// The projections the people surfaces draw, kept out of the views so the
/// same fact is derived once.
enum PeopleModel {
    static func confirmed(_ links: [PersonMeetingLink]) -> [PersonMeetingLink] {
        links.filter { $0.confidence == .confirmed }
    }

    /// Six UTC calendar-month buckets, oldest first. UTC keeps the projection
    /// deterministic at local midnight and matches the store's timestamps.
    static func cadence(_ links: [PersonMeetingLink], now: Date = .now, months: Int = 6) -> [Double] {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0) ?? .gmt
        let month = { (date: Date) -> Int in
            let parts = calendar.dateComponents([.year, .month], from: date)
            return (parts.year ?? 0) * 12 + (parts.month ?? 1) - 1
        }
        let first = month(now) - months + 1
        var values = [Double](repeating: 0, count: months)
        for link in links where link.confidence == .confirmed {
            let index = month(link.meeting.at) - first
            if index >= 0, index < months {
                values[index] += 1
            }
        }
        return values
    }

    static func lastConfirmed(_ links: [PersonMeetingLink]) -> Date? {
        confirmed(links).map(\.meeting.at).max()
    }

    /// The organizations the loaded rows already carry, with how many carry
    /// each. Derived from the list on screen rather than asked for again;
    /// sorted by name so the strip does not reorder when a meeting lands.
    static func organizations(_ entries: [PersonListEntry]) -> [(name: String, count: Int)] {
        var counts: [String: Int] = [:]
        for entry in entries {
            guard let organization = entry.person.organization else { continue }
            counts[organization, default: 0] += 1
        }
        return counts
            .map { (name: $0.key, count: $0.value) }
            .sorted { $0.name.localizedCompare($1.name) == .orderedAscending }
    }

    /// "1 meeting", "4 meetings".
    static func meetings(_ count: UInt64) -> String {
        count == 1 ? "1 meeting" : "\(count) meetings"
    }

    static func people(_ count: Int) -> String {
        count == 1 ? "1 person" : "\(count) people"
    }
}

/// One ledger line in the shape both ledgers keep it, so open loops and
/// commitments render through one section instead of two copies of it.
struct PersonLedgerRow: Identifiable {
    let id: String
    let text: String
    /// The meeting it was said in, which is also the link's label.
    let title: String
    let meetingId: String
    let at: Date
    /// When it was first raised, for a line that has outlived a meeting.
    let carriedSince: Date?
    let status: PersonLoopStatus
    /// Owed for longer than a working week.
    let stale: Bool
    let direction: PersonLoopDirection

    init(_ loop: PersonOpenLoop) {
        id = loop.loopId
        text = loop.text
        title = loop.title
        meetingId = loop.meetingId
        at = loop.at
        carriedSince = loop.carriedSinceAtUtcMs.map(PeopleFormat.date)
        status = loop.status
        stale = loop.waitingOnStale
        direction = loop.direction
    }

    init(_ commitment: PersonCommitment) {
        id = commitment.loopId
        text = commitment.text
        title = commitment.title
        meetingId = commitment.meetingId
        at = commitment.at
        carriedSince = nil
        status = commitment.status
        stale = commitment.waitingOnStale
        direction = commitment.direction
    }

    /// "Open since 4 Mar" earns its line only when the loop has outlived the
    /// room the sentence already cites.
    var carriedNote: String? {
        guard let carriedSince, carriedSince != at else { return nil }
        return "Open since \(PeopleFormat.moment(carriedSince))"
    }
}

extension Array where Element == PersonLedgerRow {
    /// What the user owes. Bucketing on `mine` keeps the split total, so a row
    /// can never fall out of a page by being neither.
    var mine: [PersonLedgerRow] { filter { $0.direction == .mine } }
    var waitingOn: [PersonLedgerRow] { filter { $0.direction != .mine } }
}

/// Dates and counts as the people pages say them.
enum PeopleFormat {
    static func date(_ milliseconds: Int64) -> Date {
        Date(timeIntervalSince1970: Double(milliseconds) / 1000)
    }

    /// "Today 14:32", "12 Mar 09:15": the day a reader can place, then the
    /// clock time that tells two meetings on it apart.
    static func moment(_ date: Date) -> String {
        let days = Calendar.current.dateComponents(
            [.day], from: Calendar.current.startOfDay(for: date), to: Calendar.current.startOfDay(for: .now)
        ).day ?? 0
        let day = days >= 0 && days < 7 ? date.relativeDay : date.short
        return "\(day) \(date.time)"
    }

    /// "Last met 3 days ago", falling back to the date once the elapsed
    /// phrasing stops being the fact somebody scans a list for.
    static func elapsed(_ date: Date) -> String {
        let seconds = max(0, Date.now.timeIntervalSince(date))
        if seconds < 60 { return "just now" }
        if seconds < 3600 { return "\(Int(seconds / 60)) min ago" }
        if seconds < 86400 { return "\(Int(seconds / 3600)) h ago" }
        let days = Int(seconds / 86400)
        if days < 30 { return days == 1 ? "yesterday" : "\(days) days ago" }
        return moment(date)
    }

    /// "18.4%" — the talk share, from thousandths.
    static func talkShare(_ permille: UInt32) -> String {
        let share = Double(permille) / 10
        return share == share.rounded()
            ? "\(Int(share))%"
            : String(format: "%.1f%%", share)
    }
}

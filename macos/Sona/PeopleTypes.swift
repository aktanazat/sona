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
    case transcriptCorrectionConflict = "transcript_correction_conflict"
    case remoteUnavailable = "remote_unavailable"
    case engineFailure = "engine_failure"
    case importUnreadable = "import_unreadable"
    case transcriptDeleted = "transcript_deleted"

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
        case .transcriptCorrectionConflict:
            "These people learned different replacements for the same phrase. Edit or remove the conflicting learned phrase before merging."
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
        case .transcriptDeleted: "Transcript deleted based on your retention settings."
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

    /// Still owed by somebody: open, or carried forward from an earlier
    /// meeting. Done and dropped are history.
    var outstanding: Bool {
        switch self {
        case .open, .carried: true
        case .done, .dropped: false
        }
    }

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
    /// How many meetings of its series it has come up in.
    let raisedInMeetings: UInt32

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
    let quote: String?
    let atMs: Int64

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
    /// Time recorded across the confirmed meetings, in milliseconds.
    let recordedMs: UInt64?
    /// What became of the threads across the confirmed meetings.
    let ledgerOutcomes: LedgerOutcomeCounts?
    let documents: [PersonDocumentSummary]
    let contact: PersonContactCard?
    /// Plans that named this person whose time passed with no recording.
    let plansWithoutRecord: [HeardPlan]
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

/// One company on the companies list: what its page holds, counted.
struct CompanySummary: Decodable, Identifiable {
    /// The label as its people carry it.
    let name: String
    /// What `organization_detail` answers to.
    let slug: String
    let peopleCount: UInt64
    let meetingsCount: UInt64
    let lastMeetingAtUtcMs: Int64?
    let openLoopsCount: UInt64

    var id: String { slug }
}

struct CompaniesListResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    /// Most recent meeting first; companies with no meeting yet last.
    let companies: [CompanySummary]
}

/// Everything still open across the whole corpus, newest first.
struct PeopleOpenLoopsInbox: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let entries: [PersonOpenLoop]
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
    /// Meetings with this person before this one; never a later one.
    let priorMeetings: UInt64
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
    /// At most one per speaker, and only for speakers still waiting.
    let suggestions: [VoiceSpeakerSuggestion]
    /// Speakers still waiting whose voice other meetings heard, unnamed there too.
    let sameVoice: [SameVoice]
}

/// One waiting speaker and the other meetings where the same voice waits for a
/// name, oldest first. Naming the speaker names the ones kept with it too.
struct SameVoice: Decodable, Identifiable {
    let speakerId: String
    let meetings: [SameVoiceMeeting]

    var id: String { speakerId }
}

struct SameVoiceMeeting: Decodable, Identifiable {
    let sessionId: String
    let speakerId: String
    let speakerRevision: UInt64
    let title: String
    let startedAtUtcMs: Int64
    /// Where only this voice talks; nil once the recording is gone.
    let clip: VoiceClip?

    var id: String { sessionId }
}

struct VoiceClip: Decodable {
    let startOffsetNs: UInt64
    let endOffsetNs: UInt64
}

/// A speaker whose voice matches the one you named as this person in other
/// meetings, or as yourself when `personId` is nil. It names nobody until it
/// is confirmed.
struct VoiceSpeakerSuggestion: Decodable, Identifiable {
    let speakerId: String
    let personId: String?
    let displayName: String

    var id: String { speakerId }
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

/// The link picker, one page at a time: what was typed to narrow it, and
/// where the read stands. Rows already read stay on screen while the next
/// page loads or after it fails, so a slow core never blanks the list.
struct LinkPicker {
    enum State {
        case idle
        case loading(more: [LinkCandidate] = [])
        /// `next` is the cursor for the page after this one; nil means the
        /// list is exhausted.
        case loaded([LinkCandidate], next: Int64?)
        case failed(String, earlier: [LinkCandidate])

        var rows: [LinkCandidate] {
            switch self {
            case .idle: []
            case let .loading(more): more
            case let .loaded(rows, _): rows
            case let .failed(_, earlier): earlier
            }
        }
    }

    var query = ""
    var state: State = .idle
}

/// A merge stopped because both people have a saved voice from different
/// models. The merged person keeps one of them.
struct VoiceConflict {
    let targetPersonId: String
    let targetName: String
}

/// `PersonSummaryOutcome`: what pressing Regenerate did.
enum PersonSummaryOutcome: String, Decodable {
    case written
    case noEvidence = "no_evidence"
    case engineUnavailable = "engine_unavailable"
    case failed

    /// The one line under the paragraph after an attempt that wrote nothing.
    /// `written` needs no line: the new paragraph is the report.
    var note: String? {
        switch self {
        case .written: nil
        case .noEvidence: "There is no confirmed meeting to write this from yet."
        case .engineUnavailable: "No engine is ready to write this. Check Settings › Meetings."
        case .failed: "The engine answered nothing usable. The earlier paragraph stays."
        }
    }
}

struct PersonSummaryRegenerateResult: Decodable {
    let outcome: PersonSummaryOutcome
    let page: PersonDetailResult
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
/// off and forget the voice. A label also names the same voice in `sameVoice`.
enum VoiceIdentityAction: Encodable {
    case label(VoiceIdentityTarget, sameVoice: [SameVoiceSpeaker])
    case correctTo(VoiceIdentityTarget)
    case markUnknown

    private enum CodingKeys: String, CodingKey {
        case kind
        case target
        case sameVoice = "same_voice"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .label(target, sameVoice):
            try container.encode("label", forKey: .kind)
            try container.encode(target, forKey: .target)
            try container.encode(sameVoice, forKey: .sameVoice)
        case let .correctTo(target):
            try container.encode("correct_to", forKey: .kind)
            try container.encode(target, forKey: .target)
        case .markUnknown:
            try container.encode("mark_unknown", forKey: .kind)
        }
    }
}

/// A speaker of another meeting a label names too, at the revision the sheet
/// showed.
struct SameVoiceSpeaker: Encodable {
    let sessionId: String
    let speakerId: String
    let expectedSpeakerRevision: UInt64

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case speakerId = "speaker_id"
        case expectedSpeakerRevision = "expected_speaker_revision"
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

enum PersonContributionAttribution: String, Decodable {
    case confirmed
    case recognized
}

struct PersonContribution: Decodable, Identifiable {
    let meetingId: String
    let meetingTitle: String
    let meetingAtUtcMs: Int64
    let transcriptRevisionId: String
    let segmentId: String
    let trackId: String
    let speakerId: String
    let attribution: PersonContributionAttribution
    let linkConfidence: PersonLinkConfidence
    let startOffsetNs: UInt64
    let endOffsetNs: UInt64
    let rawText: String
    let correctedText: String?

    var id: String { segmentId }
    var at: Date { PeopleFormat.date(meetingAtUtcMs) }
    var startMilliseconds: UInt64 { startOffsetNs / 1_000_000 }
    var timestamp: String {
        let seconds = startOffsetNs / 1_000_000_000
        return String(format: "%llu:%02llu", seconds / 60, seconds % 60)
    }
}

struct PersonContributionsResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let entries: [PersonContribution]
    let nextOffset: UInt64?
}

struct PersonAvatar: Decodable {
    let pngBase64: String
    let updatedAtUtcMs: Int64
    let provenance: PersonPhotoProvenance?
}

struct PersonAvatarResult: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let avatar: PersonAvatar?
}

struct PersonAvatarSetRequest: Encodable {
    let personId: String
    let expectedRevision: UInt64
    let pngBase64: String?
    let provenance: PersonPhotoProvenance?
    let originalBase64: String?

    enum CodingKeys: String, CodingKey {
        case personId = "person_id"
        case expectedRevision = "expected_revision"
        case pngBase64 = "png_base64"
        case provenance
        case originalBase64 = "original_base64"
    }
}

/// The fields of the one contact the user chose in the Contacts picker, kept
/// beside the person. Sona never reads the address book itself.
struct PersonContactCard: Codable, Equatable {
    let name: String
    let emails: [PersonContactValue]
    let phones: [PersonContactValue]
    let organization: String?
    let jobTitle: String?
    /// Formatted the way Contacts formats them, one line per row.
    let addresses: [PersonContactValue]
    let urls: [PersonContactValue]
    let birthday: PersonContactBirthday?

    private enum WireKeys: String, CodingKey {
        case name, emails, phones, organization, addresses, urls, birthday
        case jobTitle = "job_title"
    }

    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: WireKeys.self)
        try values.encode(name, forKey: .name)
        try values.encode(emails, forKey: .emails)
        try values.encode(phones, forKey: .phones)
        try values.encodeIfPresent(organization, forKey: .organization)
        try values.encodeIfPresent(jobTitle, forKey: .jobTitle)
        try values.encode(addresses, forKey: .addresses)
        try values.encode(urls, forKey: .urls)
        try values.encodeIfPresent(birthday, forKey: .birthday)
    }
}

struct PersonContactValue: Codable, Equatable, Hashable {
    /// Contacts' own label, already localized: "work", "mobile".
    let label: String?
    let value: String
}

struct PersonContactBirthday: Codable, Equatable {
    let month: UInt8
    let day: UInt8
    let year: Int32?

    /// "29 February", or "29 February 1990" when the year is known.
    var text: String {
        var parts = DateComponents()
        parts.calendar = Calendar(identifier: .gregorian)
        parts.year = year.map(Int.init) ?? 2000
        parts.month = Int(month)
        parts.day = Int(day)
        guard let date = parts.date else { return "\(day)/\(month)" }
        return year == nil
            ? date.formatted(.dateTime.day().month(.wide))
            : date.formatted(.dateTime.day().month(.wide).year())
    }
}

struct PersonContactSetRequest: Encodable {
    let personId: String
    let expectedRevision: UInt64
    /// Nil takes the card off the person.
    let contact: PersonContactCard?

    enum CodingKeys: String, CodingKey {
        case personId = "person_id"
        case expectedRevision = "expected_revision"
        case contact
    }
}

// MARK: - Reading the corpus

/// The projections the people surfaces draw, kept out of the views so the
/// same fact is derived once.
enum PeopleModel {
    static func confirmed(_ links: [PersonMeetingLink]) -> [PersonMeetingLink] {
        links.filter { $0.confidence == .confirmed }
    }

    /// Confirmed meetings in each of the last `months` calendar months, oldest
    /// first. The months are the user's own, so a meeting late on the last
    /// evening of a month counts in the month they remember it in.
    static func cadence(_ links: [PersonMeetingLink], now: Date = .now, months: Int = 6) -> [PersonMonthCount] {
        let calendar = Calendar.current
        guard let current = calendar.dateInterval(of: .month, for: now)?.start else { return [] }
        let starts = (0..<months).compactMap {
            calendar.date(byAdding: .month, value: $0 - months + 1, to: current)
        }
        var counts = [Int](repeating: 0, count: starts.count)
        for link in links where link.confidence == .confirmed {
            guard let start = calendar.dateInterval(of: .month, for: link.meeting.at)?.start,
                  let index = starts.firstIndex(of: start) else { continue }
            counts[index] += 1
        }
        return zip(starts, counts).map { PersonMonthCount(start: $0, count: $1) }
    }

    /// Whether a confirmed meeting fell on each of the last `days` days,
    /// oldest first and today last.
    static func recentDays(_ links: [PersonMeetingLink], now: Date = .now, days: Int = 14) -> [Bool] {
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: now)
        var met = [Bool](repeating: false, count: days)
        for link in links where link.confidence == .confirmed {
            let back = calendar.dateComponents([.day], from: calendar.startOfDay(for: link.meeting.at), to: today).day ?? -1
            if back >= 0, back < days {
                met[days - 1 - back] = true
            }
        }
        return met
    }

    static func lastConfirmed(_ links: [PersonMeetingLink]) -> Date? {
        confirmed(links).map(\.meeting.at).max()
    }

    /// The usual number of days between meetings: the median gap between
    /// the distinct days of confirmed meetings. Nil under four such days,
    /// where a median says nothing yet.
    static func usualGapDays(_ links: [PersonMeetingLink]) -> Int? {
        let calendar = Calendar.current
        let days = Set(confirmed(links).map { calendar.startOfDay(for: $0.meeting.at) }).sorted()
        guard days.count >= 4 else { return nil }
        let gaps = zip(days, days.dropFirst())
            .compactMap { calendar.dateComponents([.day], from: $0, to: $1).day }
            .sorted()
        let middle = gaps.count / 2
        return gaps.count.isMultiple(of: 2) ? (gaps[middle - 1] + gaps[middle] + 1) / 2 : gaps[middle]
    }

    /// "1 open item", "3 open items".
    static func openItems(_ count: UInt64) -> String {
        count == 1 ? "1 open item" : "\(count) open items"
    }

    /// "1 meeting", "4 meetings".
    static func meetings(_ count: UInt64) -> String {
        count == 1 ? "1 meeting" : "\(count) meetings"
    }

    static func people(_ count: Int) -> String {
        count == 1 ? "1 person" : "\(count) people"
    }
}

/// One calendar month of meetings with a person.
struct PersonMonthCount: Identifiable, Equatable {
    let start: Date
    let count: Int

    var id: Date { start }
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

    /// How long ago, as a number and the words after it: ("3", "days ago").
    /// Today and yesterday are words of their own.
    static func since(_ date: Date, now: Date = .now) -> (value: String, unit: String?) {
        let calendar = Calendar.current
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date), to: calendar.startOfDay(for: now)).day ?? 0
        switch days {
        case ..<1: return ("Today", nil)
        case 1: return ("Yesterday", nil)
        case ..<14: return ("\(days)", "days ago")
        case ..<61: return ("\(days / 7)", "weeks ago")
        case ..<730: return ("\(days / 30)", "months ago")
        default: return ("\(days / 365)", "years ago")
        }
    }

    /// "18.4%" — the talk share, from thousandths.
    static func talkShare(_ permille: UInt32) -> String {
        let share = Double(permille) / 10
        return share == share.rounded()
            ? "\(Int(share))%"
            : String(format: "%.1f%%", share)
    }
}

import Foundation
import Observation

/// Which of the three people screens is up. An organization is not a fourth
/// kind of noun: it is a slice of the list, reached from a label on it or on a
/// person's page.
enum PeopleRoute: Equatable {
    case list
    case person(String)
    case organization(String)
}

/// Something the shell itself refused, in the one sentence a reader can act on.
/// Used where there is no remote error to quote: a label that committed while
/// the voice behind it was not kept.
struct PersonFailure: LocalizedError {
    let message: String

    var errorDescription: String? { message }
}

/// People, one person, one organization, and everything the corpus answers
/// about them.
///
/// One store for the three screens because they are one read of one corpus:
/// every write returns the revision the next write must carry, and the two
/// meeting events that change a person change the list and the page alike.
/// Reads are guarded by a generation counter, so an answer to a question the
/// screen has moved on from never lands.
@MainActor
@Observable
final class PeopleStore {
    /// Everybody Sona knows, or nil until the first read answers.
    private(set) var entries: [PersonListEntry]?
    /// The list could not be read. Not the same answer as an empty corpus.
    private(set) var listFailed = false
    /// The corpus revision the list was read at.
    private(set) var revision: UInt64 = 0

    private(set) var detail: PersonDetail?
    private(set) var detailFailed = false
    /// The revision `person_detail` answered with, which is what this page's
    /// writes carry.
    private(set) var detailRevision: UInt64 = 0
    /// The relationship brief for the open person: how often you have met and
    /// what is still open, in one line each.
    private(set) var briefing: BriefingRow?

    private(set) var organization: OrganizationDetail?
    private(set) var organizationFailed = false

    /// What is still open across everybody, newest first.
    private(set) var inbox: [PersonOpenLoop] = []
    /// Terms Sona keeps hearing and cannot spell.
    private(set) var candidates: [PeopleVocabularyCandidate] = []
    /// Meetings offered for a manual link, newest first. Nil while the picker
    /// is still reading them.
    private(set) var linkCandidates: [LinkCandidate]?

    private(set) var route: PeopleRoute = .list
    /// A write is out. Every verb on the page is held until it answers.
    private(set) var busy = false
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?

    @ObservationIgnored private let core: Core
    /// Reads are stamped: an answer for a person the screen has left is
    /// dropped rather than drawn over the one it moved to.
    @ObservationIgnored private var generation: UInt64 = 0
    /// How many rows the inbox asks for, as the web app asked for them.
    @ObservationIgnored private let inboxLimit = 5

    init(core: Core) {
        self.core = core
        // A meeting's ledger is what a person's loops are read out of, and a
        // deleted meeting takes its links with it, so both re-read whatever is
        // on screen.
        core.observe(CoreEvent.peopleArtifactChanged) { [weak self] _ in self?.reload() }
        core.observe(CoreEvent.peopleMeetingRemoved) { [weak self] _ in self?.reload() }
    }

    /// The first load, once the core is up.
    func start() async {
        await reread()
    }

    // MARK: - Navigation

    func openPerson(_ personId: String) {
        guard route != .person(personId) else { return }
        route = .person(personId)
        clearPerson()
        reload()
    }

    func openOrganization(_ name: String) {
        guard route != .organization(name) else { return }
        route = .organization(name)
        organization = nil
        organizationFailed = false
        reload()
    }

    /// Back to the list, and re-read it: what a page's writes changed is what
    /// the list now says about that person.
    func toList() {
        route = .list
        clearPerson()
        organization = nil
        organizationFailed = false
        reload()
    }

    /// Re-reads whatever the current route needs, plus the two lists under the
    /// list screen.
    func reload() {
        Task { await reread() }
    }

    /// One read of everything on screen. The requests go out together and
    /// their answers land as they arrive.
    private func reread() async {
        generation += 1
        let stamp = generation
        switch route {
        case .list:
            async let list: Void = loadList(stamp)
            async let inbox: Void = loadInbox(stamp)
            async let candidates: Void = loadCandidates(stamp)
            _ = await (list, inbox, candidates)
        case let .person(personId):
            async let detail: Void = loadDetail(personId, stamp)
            async let briefing: Void = loadBriefing(personId, stamp)
            async let list: Void = loadList(stamp)
            _ = await (detail, briefing, list)
        case let .organization(name):
            async let organization: Void = loadOrganization(name, stamp)
            async let list: Void = loadList(stamp)
            _ = await (organization, list)
        }
    }

    private func clearPerson() {
        detail = nil
        briefing = nil
        detailFailed = false
        linkCandidates = nil
    }

    // MARK: - Reads

    private func loadList(_ stamp: UInt64) async {
        do {
            let result: PeopleListResult = try await core.request("people_list")
            guard stamp == generation else { return }
            entries = result.entries
            revision = result.revision
            listFailed = false
        } catch {
            guard stamp == generation else { return }
            listFailed = true
        }
    }

    private func loadDetail(_ personId: String, _ stamp: UInt64) async {
        do {
            let result: PersonDetailResult = try await core.request("person_detail", ["personId": personId])
            guard stamp == generation else { return }
            detail = result.detail
            detailRevision = result.revision
            detailFailed = false
        } catch {
            guard stamp == generation else { return }
            detailFailed = true
        }
    }

    /// The brief for one person: `person_context` answers for a set of them,
    /// and a person's own page is the set of one. A person you have never met
    /// has nothing to brief, and the row says so by counting zero.
    private func loadBriefing(_ personId: String, _ stamp: UInt64) async {
        do {
            let result: BriefingResult = try await core.request("person_context", ["personIds": [personId]])
            guard stamp == generation else { return }
            briefing = result.rows.first { $0.meetingsCount > 0 }
        } catch {
            guard stamp == generation else { return }
            briefing = nil
        }
    }

    /// The label on a chip and the slug in a link are the same key: the
    /// command slugifies whatever it is given, so neither side derives one.
    private func loadOrganization(_ name: String, _ stamp: UInt64) async {
        do {
            let result: OrganizationDetailResult = try await core.request("organization_detail", ["slug": name])
            guard stamp == generation else { return }
            organization = result.detail
            organizationFailed = false
        } catch {
            guard stamp == generation else { return }
            organizationFailed = true
        }
    }

    private func loadInbox(_ stamp: UInt64) async {
        do {
            let result: PeopleOpenLoopsInbox = try await core.request("open_loops_inbox", ["limit": inboxLimit])
            guard stamp == generation else { return }
            inbox = result.entries
        } catch {
            guard stamp == generation else { return }
            inbox = []
        }
    }

    private func loadCandidates(_ stamp: UInt64) async {
        do {
            let result: PeopleVocabularyCandidates = try await core.request("vocabulary_candidates")
            guard stamp == generation else { return }
            candidates = result.entries
        } catch {
            guard stamp == generation else { return }
            candidates = []
        }
    }

    /// Answers one mined term with "no". The store remembers the answer, so
    /// the re-read after it is what takes the row off the list — nothing here
    /// removes it locally.
    func dismissCandidate(_ text: String) {
        act { [self] in
            let decision = PeopleLearningDecision(candidateKey: text, status: "dismissed")
            try await core.request("learning_decide", ["request": decision])
            await loadCandidates(generation)
        }
    }

    /// The meetings a manual link can reach: the newest ones this person is
    /// not on already.
    func loadLinkCandidates() {
        linkCandidates = nil
        act { [self] in
            let page: LinkCandidatePage = try await core.request("meeting_list", ["limit": 50])
            let linked = Set((detail?.links ?? []).map(\.id))
            linkCandidates = page.entries.filter { !linked.contains($0.sessionId) }
        }
    }

    /// Everybody in one meeting's roster, read against every meeting before
    /// it. The band a meeting shows above its notes.
    func meetingContext(_ sessionId: String) async -> [PersonMeetingContextRow] {
        do {
            let result: PersonMeetingContextResult = try await core.request(
                "meeting_people_context", ["sessionId": sessionId])
            error = nil
            return result.rows
        } catch {
            self.error = Self.message(error)
            return []
        }
    }

    // MARK: - Writes

    /// A rename is one value with a receipt behind it, so there is no
    /// confirmation and no Save: the field commits when it is left.
    func rename(to displayName: String) {
        guard case let .person(personId) = route else { return }
        write { revision, core in
            let request = PersonRenameRequest(
                personId: personId, displayName: displayName, expectedRevision: revision)
            return try await core.request("person_rename", ["request": request])
        }
    }

    /// Merging keeps both records' samples, which is what merging two records
    /// of one person means, and follows whatever the merge left behind.
    func merge(into targetPersonId: String) {
        guard case let .person(personId) = route else { return }
        write(
            { revision, core in
                let request = PersonMergeRequest(
                    sourcePersonId: personId,
                    targetPersonId: targetPersonId,
                    expectedRevision: revision,
                    voiceProfileResolution: .combineCompatible)
                return try await core.request("person_merge", ["request": request])
            },
            then: { store, result in
                store.follow(result.person?.id ?? targetPersonId)
            })
    }

    func delete() {
        guard case let .person(personId) = route else { return }
        write(
            { revision, core in
                let request = PersonDeleteRequest(personId: personId, expectedRevision: revision)
                return try await core.request("person_delete", ["request": request])
            },
            then: { store, _ in store.toList() })
    }

    /// Moves the chosen evidence onto another person, new or existing, and
    /// follows a newly created one.
    func split(
        to target: PersonSplitTarget,
        meetingIds: [String],
        aliases: [String],
        calendarEmails: [String],
        documentIds: [String]
    ) {
        guard case let .person(personId) = route else { return }
        write(
            { revision, core in
                let request = PersonSplitRequest(
                    sourcePersonId: personId,
                    target: target,
                    meetingIds: meetingIds,
                    aliases: aliases,
                    calendarEmails: calendarEmails,
                    documentIds: documentIds,
                    expectedRevision: revision)
                return try await core.request("person_split", ["request": request])
            },
            then: { store, result in
                guard let split = result.person?.id, split != personId else {
                    store.reload()
                    return
                }
                store.follow(split)
            })
    }

    func confirmLink(_ meetingId: String) {
        link("link_confirm", meetingId)
    }

    func removeLink(_ meetingId: String) {
        link("link_remove", meetingId)
    }

    /// You say this meeting was with this person: the strongest evidence there
    /// is, and the only kind Sona never guesses at.
    func addManualLink(_ meetingId: String) {
        linkCandidates = nil
        link("link_add_manual", meetingId)
    }

    private func link(_ method: String, _ meetingId: String) {
        guard case let .person(personId) = route else { return }
        write { revision, core in
            let request = LinkRequest(
                meetingId: meetingId, personId: personId, expectedRevision: revision)
            return try await core.request(method, ["request": request])
        }
    }

    /// One model call over this person's own evidence. A Mac with no engine
    /// refuses outright, which is why success says nothing.
    func regenerateSummary() {
        guard case let .person(personId) = route else { return }
        busy = true
        act { [self] in
            defer { busy = false }
            let result: PersonDetailResult = try await core.request(
                "person_summary_regenerate", ["personId": personId])
            guard route == .person(personId) else { return }
            detail = result.detail
            detailRevision = result.revision
        }
    }

    /// Sona stops recognising this voice on this Mac. The person's name and
    /// their meetings stay.
    func removeVoiceProfile() {
        guard case let .person(personId) = route else { return }
        busy = true
        act { [self] in
            defer { busy = false }
            let request = VoiceProfileRemovalRequest(
                personId: personId, expectedPeopleRevision: detailRevision)
            let _: VoiceProfileEnrollmentStatus = try await core.request(
                "voice_remove_profile", ["request": request])
            await reread()
        }
    }

    // MARK: - Machinery

    /// Runs one write at the revision the open page was read at, then re-reads.
    /// `then` replaces that re-read for the two verbs that move the page
    /// somewhere else, and owns re-reading wherever it lands.
    private func write(
        _ operation: @escaping (UInt64, Core) async throws -> PeopleMutationResult,
        then: ((PeopleStore, PeopleMutationResult) -> Void)? = nil
    ) {
        busy = true
        act { [self] in
            defer { busy = false }
            do {
                let result = try await operation(detailRevision, core)
                if let then {
                    then(self, result)
                } else {
                    await reread()
                }
            } catch {
                // A refusal is itself a fact about the corpus — usually that
                // it moved — so the page re-reads before the reader decides
                // what to do about it.
                await reread()
                throw error
            }
        }
    }

    /// Opens another person after a merge or a split moved this page.
    private func follow(_ personId: String) {
        route = .person(personId)
        clearPerson()
        reload()
    }

    /// Runs one action: clears the error it started with on success, and on
    /// failure keeps the sentence the command earned.
    private func act(_ work: @escaping @MainActor () async throws -> Void) {
        Task {
            do {
                try await work()
                error = nil
            } catch {
                self.error = Self.message(error)
            }
        }
    }

    /// The core's own refusal where there is one, and what the transport said
    /// otherwise.
    static func message(_ error: Error) -> String {
        if let error = error as? CoreError, let refusal = error.remote(as: PeopleCommandError.self) {
            return refusal.message
        }
        return error.localizedDescription
    }
}

/// The speakers in one meeting that Sona cannot put a name to, and the three
/// answers a reader can give: this is that person, no it is this one, or take
/// the name off and forget the voice.
///
/// Per session, so the shell can hold one beside a meeting without the people
/// list paying for it. Every revision a request carries is read back
/// immediately before that request: the identify half moves the meeting and
/// the corpus on, so a second write from an older snapshot would be refused.
@MainActor
@Observable
final class VoiceIdentityStore {
    /// Which question the sheet is asking.
    enum Question: Equatable {
        /// A speaker with nobody behind them yet.
        case label(String)
        /// A speaker whose person is wrong.
        case correct(String)

        var speakerId: String {
            switch self {
            case let .label(speakerId), let .correct(speakerId): speakerId
            }
        }

        var isCorrection: Bool {
            if case .correct = self { return true }
            return false
        }
    }

    /// The meeting whose speakers this is about.
    private(set) var sessionId: String?
    /// Every speaker the meeting has, under the name the transcript carries.
    private(set) var speakers: [VoiceIdentitySpeaker] = []
    /// The ones with nobody behind them yet.
    private(set) var unresolved: [VoiceIdentitySpeaker] = []
    /// Who a label can point at. Nil until the read answers.
    private(set) var people: [PersonListEntry]?
    private(set) var peopleFailed = false
    private(set) var question: Question?
    /// Marking a speaker unknown deletes the samples kept from it and cannot
    /// be taken back, so the sheet asks first.
    private(set) var confirmingUnknown = false
    private(set) var busy = false
    private(set) var error: String?

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.peopleArtifactChanged) { [weak self] _ in
            guard let self, let sessionId else { return }
            Task { await self.load(sessionId) }
        }
    }

    /// Nothing to read until a session is named; the shell calls `load`.
    func start() async {}

    /// Reads one meeting's speakers, which of them are unresolved, and who
    /// they could be.
    func load(_ sessionId: String) async {
        self.sessionId = sessionId
        do {
            try await reread(sessionId)
            error = nil
        } catch {
            self.error = PeopleStore.message(error)
        }
        await loadPeople()
    }

    private func reread(_ sessionId: String) async throws {
        do {
            let status: VoiceIdentityStatus = try await core.request(
                "voice_identity_status", ["sessionId": sessionId])
            let meeting: VoiceIdentityMeeting = try await core.request("meeting_get", ["sessionId": sessionId])
            speakers = meeting.speakers
            let waiting = Set(status.unresolvedActiveSpeakerIds)
            // A status id can name a speaker this read has not seen yet; a
            // count nobody can act on is worse than one refresh late.
            unresolved = meeting.speakers.filter { waiting.contains($0.speakerId) }
        } catch {
            speakers = []
            unresolved = []
            throw error
        }
    }

    func loadPeople() async {
        do {
            let result: PeopleListResult = try await core.request("people_list")
            people = result.entries
            peopleFailed = false
        } catch {
            peopleFailed = true
        }
    }

    /// Opens the sheet on the first speaker still waiting for a name.
    func askNext() {
        guard let speaker = unresolved.first else { return }
        ask(.label(speaker.speakerId))
    }

    func askCorrection(_ speakerId: String) {
        guard speakers.contains(where: { $0.speakerId == speakerId }) else { return }
        ask(.correct(speakerId))
    }

    /// Every route that changes which speaker the sheet asks about also drops
    /// a pending "mark unknown", so an answer meant for one speaker can never
    /// land on the next.
    func ask(_ question: Question?) {
        confirmingUnknown = false
        self.question = question
    }

    func requestUnknown() {
        confirmingUnknown = true
    }

    func cancelUnknown() {
        confirmingUnknown = false
    }

    func speaker(_ speakerId: String) -> VoiceIdentitySpeaker? {
        speakers.first { $0.speakerId == speakerId }
    }

    /// Names the speaker, and remembers the voice when asked to.
    ///
    /// The label commits first and moves the meeting on, so a failure in the
    /// enrollment half must not skip the re-read: the sheet would sit on a
    /// speaker that is already named, and the next save would carry a revision
    /// the store has left behind.
    func save(_ target: VoiceIdentityTarget, remember: Bool) {
        guard let question, let sessionId else { return }
        busy = true
        act { [self] in
            defer { busy = false }
            let resolved = try await identify(
                sessionId: sessionId,
                speakerId: question.speakerId,
                action: question.isCorrection ? .correctTo(target) : .label(target))
            var failure: Error?
            if remember, let personId = resolved {
                do {
                    try await enroll(personId: personId, sessionId: sessionId, speakerId: question.speakerId)
                } catch {
                    failure = error
                }
            }
            try await reread(sessionId)
            // A correction answers one speaker; labelling walks the queue, so
            // the sheet moves on to whoever is still unnamed.
            ask(question.isCorrection ? nil : unresolved.first.map { .label($0.speakerId) })
            if let failure { throw failure }
        }
    }

    /// Takes the name off this speaker and deletes the samples kept from it.
    func markUnknown() {
        guard let question, let sessionId else { return }
        busy = true
        act { [self] in
            defer { busy = false }
            _ = try await identify(
                sessionId: sessionId, speakerId: question.speakerId, action: .markUnknown)
            try await reread(sessionId)
            ask(nil)
        }
    }

    private func identify(
        sessionId: String, speakerId: String, action: VoiceIdentityAction
    ) async throws -> String? {
        let people: PeopleListResult = try await core.request("people_list")
        let meeting: VoiceIdentityMeeting = try await core.request("meeting_get", ["sessionId": sessionId])
        let request = VoiceIdentityRequest(
            operationId: UUID().uuidString,
            requestedAtUtcMs: Int64(Date.now.timeIntervalSince1970 * 1000),
            sessionId: sessionId,
            expectedMeetingRevision: meeting.session.revision,
            expectedPeopleRevision: people.revision,
            speakerId: speakerId,
            action: action)
        let result: VoiceIdentityResult = try await core.request(
            "voice_identify_speaker", ["request": request])
        return result.resolvedPersonId
    }

    /// Enrollment needs the speaker's own revision too, and the identify half
    /// has just moved all three, so every one of them is read again here.
    private func enroll(personId: String, sessionId: String, speakerId: String) async throws {
        let people: PeopleListResult = try await core.request("people_list")
        let meeting: VoiceIdentityMeeting = try await core.request("meeting_get", ["sessionId": sessionId])
        guard let speaker = meeting.speakers.first(where: { $0.speakerId == speakerId }) else {
            throw PersonFailure(message: PeopleCommandError.insufficientEnrollmentEvidence.message)
        }
        let request = VoiceProfileEnrollmentRequest(
            personId: personId,
            sessionId: sessionId,
            speakerId: speakerId,
            expectedMeetingRevision: meeting.session.revision,
            expectedSpeakerRevision: speaker.revision,
            expectedPeopleRevision: people.revision,
            consentVersion: 1)
        let status: VoiceProfileEnrollmentStatus = try await core.request(
            "voice_enroll_profile", ["request": request])
        // The label committed and nothing was kept: the one outcome with no
        // refusal behind it, so it says exactly what the refusals say.
        guard status.enrolled else {
            throw PersonFailure(message: PeopleCommandError.insufficientEnrollmentEvidence.message)
        }
    }

    private func act(_ work: @escaping @MainActor () async throws -> Void) {
        Task {
            do {
                try await work()
                error = nil
            } catch {
                self.error = PeopleStore.message(error)
            }
        }
    }
}

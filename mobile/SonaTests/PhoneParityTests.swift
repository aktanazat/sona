import AVFoundation
import Foundation
import Testing

final class PhoneParityTests {
    private let directory: URL
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    init() throws {
        directory = FileManager.default.temporaryDirectory.appending(path: "phone-parity-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }
    deinit { try? FileManager.default.removeItem(at: directory) }

    @Test("A dead or expired microphone never looks ready to the keyboard")
    func microphoneLease() {
        let ready = KeyboardSessionState(phase: .ready, expiresAt: now.addingTimeInterval(60), heartbeat: now)
        #expect(ready.isWarm(at: now.addingTimeInterval(5)))
        #expect(!ready.isWarm(at: now.addingTimeInterval(6)))
        #expect(!ready.isWarm(at: now.addingTimeInterval(-6)))
        var expired = ready
        expired.heartbeat = now.addingTimeInterval(60)
        #expect(!expired.isWarm(at: now.addingTimeInterval(60)))
        var failed = ready
        failed.phase = .failed
        #expect(!failed.isWarm(at: now))
    }

    @Test("Keyboard signals are consumed once and stale starts cannot open the microphone")
    func commandConsumption() throws {
        let store = KeyboardSessionStore(directory: directory)
        let command = KeyboardCommand(id: UUID(), action: .start, requestID: UUID(), documentID: UUID(), createdAt: now)
        try store.send(command)
        #expect(try store.takeCommand(now: now.addingTimeInterval(14)) == command)
        #expect(try store.takeCommand(now: now) == nil)
        try store.send(command)
        #expect(try store.takeCommand(now: now.addingTimeInterval(15)) == nil)
        #expect(try store.takeCommand(now: now) == nil)
        let draft = try KeyboardDraftStore(directory: directory).save(text: "The answer is yes.", documentID: command.documentID, now: now)
        #expect(try KeyboardDraftStore(directory: directory).load(now: now).waiting?.documentID == command.documentID)
        #expect(try KeyboardDraftStore(directory: directory).take(id: draft.id, now: now) == "The answer is yes.")
    }

    @Test("Desktop writing preferences preserve word boundaries and do not expand their own output")
    func writingProfile() throws {
        let bytes = Data(#"{"version":1,"vocabulary":[{"spoken":"sowna","written":"Sona"}],"replacements":[{"spoken":"oak tree","written":"Redwood","enabled":true},{"spoken":"oak","written":"Tree","enabled":true},{"spoken":"red","written":"blue","enabled":true},{"spoken":"blue","written":"green","enabled":true}],"snippets":[{"id":"signature","trigger":"my signature","expansion":"Regards,\nAda","enabled":true}],"styles":[]}"#.utf8)
        let profile = try JSONDecoder().decode(DictationProfile.self, from: bytes)
        #expect(profile.apply(to: "OAK TREE, oakland, red. Sowna: my signature") == "Redwood, oakland, blue. Sona: Regards,\nAda")
        #expect(profile.recognitionHints == ["Sona", "my signature"])
    }

    @Test("A rambled dictation loses its hesitations and restarts, and its spoken ordinals become a list")
    func rambleCleanup() {
        // Apple's on-device recognizer on a synthesized 26-second ramble.
        let heard = "So I wanted to. Ah, send a quick note to Dana about the launch. So basically we are moving the beta to Thursday. No wait. Friday, because the, um, the screenshots are not done yet, and there are 3 things left. 1st the pricing page, 2nd the, ah, the FAQ, and 3rd the onboarding email. Um, can you, like, let me know by tomorrow if that works? Thanks."
        #expect(DictationCleanup.clean(heard, language: "en-US") == """
            So I wanted to. Send a quick note to Dana about the launch. So basically we are moving the beta to Thursday. No wait. Friday, because the screenshots are not done yet, and there are 3 things left:
            1. The pricing page
            2. The FAQ
            3. The onboarding email

            Can you, like, let me know by tomorrow if that works? Thanks.
            """)
        #expect(DictationCleanup.clean("First, fix the login bug. Second, raise the price to 9.99 tonight.", language: "en-US") == """
            1. Fix the login bug
            2. Raise the price to 9.99 tonight
            """)
    }

    @Test("Words that only look like fillers or lists are left as said")
    func cleanupLeavesWords() {
        for (said, language) in [
            ("Vou comprar um carro amanhã.", "pt-BR"),
            ("First, check the logs.", "en-US"),
            ("Second the FAQ, then first the pricing page.", "en-US"),
            ("I know that that works.", "en-US"),
            ("Very, very good work on the launch.", "en-US"),
            ("No, no, that's the old number.", "en-US"),
            ("First of all, thanks for the quick turnaround. Second, can you send the deck?", "en-US"),
        ] {
            #expect(DictationCleanup.clean(said, language: language) == said)
        }
    }

    @Test("A style rewrite answered as the cleanup contract's JSON pastes only its words")
    func rewriteReplyUnwrapped() {
        #expect(DictationCleanup.rewritten(#"{"transcription": "We ship Friday."}"#) == "We ship Friday.")
        #expect(DictationCleanup.rewritten(#"{"note": "We ship Friday."}"#) == #"{"note": "We ship Friday."}"#)
        #expect(DictationCleanup.rewritten("We ship Friday.") == "We ship Friday.")
    }

    @Test("Phone notes display the current transcript with its latest edits and speaker names")
    func currentMeetingProjection() throws {
        let bytes = Data(#"{"format_version":1,"session":{"session_id":"meeting","title":"Planning","created_at_utc_ms":1700000000000,"current_transcript_revision_id":"new","current_diarization_generation_id":"latest"},"artifact_revisions":[{"state":"superseded","generated_at_utc_ms":1,"content":{"summary":{"text":"Old"},"outline":[],"decisions":[],"action_items":[],"key_questions":[],"risks":[],"follow_up_draft":{"text":""}}},{"state":"current","generated_at_utc_ms":2,"content":{"summary":{"text":"Ship on Friday."},"outline":[],"decisions":[],"action_items":[],"key_questions":[],"risks":[],"follow_up_draft":{"text":""}}}],"transcript_segments":[{"segment_id":"old","transcript_revision_id":"old","start_offset_ns":0,"speaker_id":"a","base_text":"Out of date."},{"segment_id":"keep","transcript_revision_id":"new","start_offset_ns":65000000000,"speaker_id":"a","base_text":"Original."},{"segment_id":"remove","transcript_revision_id":"new","start_offset_ns":70000000000,"speaker_id":"a","base_text":"Private."}],"segment_edits":[{"segment_id":"keep","edit_sequence":2,"replacement_text":"Corrected.","removed":false},{"segment_id":"keep","edit_sequence":1,"replacement_text":"Earlier edit.","removed":false},{"segment_id":"remove","edit_sequence":1,"replacement_text":"","removed":true}],"speakers":[{"speaker_id":"a","display_name":"Speaker"},{"speaker_id":"b","display_name":"Ada"}],"diarization_assignments":[{"generation_id":"earlier","segment_id":"keep","speaker_id":"a"},{"generation_id":"latest","segment_id":"keep","speaker_id":"b"}],"user_notes":{"body":"Ask about launch."}}"#.utf8)
        var meeting = try JSONDecoder().decode(PhoneMeeting.self, from: bytes)
        #expect(meeting.notes == "Ship on Friday.")
        #expect(meeting.transcript == "01:05  Ada: Corrected.")
        #expect(meeting.user_notes?.body == "Ask about launch.")
        meeting.artifact_revisions = []
        meeting.transcript_segments = []
        #expect(meeting.notes == "")
        #expect(meeting.transcript == "")
    }

    @Test("Personal notes survive the phone recording envelope without breaking older recordings")
    func recordingNotes() throws {
        let manifest = DeviceRecordingObject.manifest(deviceId: "phone", recordedAtUtcMs: 1, durationMs: 1000,
            title: "Planning", audioByteLength: 32000, audioSha256: "digest", notes: "Budget is approved.\nAsk about dates.")
        let decoded = try JSONDecoder().decode(DeviceRecordingManifest.self, from: DeviceRecordingObject.encodeManifest(manifest))
        #expect(decoded.personal_notes == "Budget is approved.\nAsk about dates.")
        let old = DeviceRecordingObject.manifest(deviceId: "phone", recordedAtUtcMs: 1, durationMs: 1000,
            title: "Planning", audioByteLength: 32000, audioSha256: "digest")
        #expect(try JSONDecoder().decode(DeviceRecordingManifest.self, from: DeviceRecordingObject.encodeManifest(old)).personal_notes == nil)
    }

    @MainActor @Test("A kept recording remains playable after the upload source is removed")
    func retainedPlayback() throws {
        let source = directory.appending(path: "recording.pcm")
        let samples = Data(repeating: 1, count: 32000)
        try samples.write(to: source)
        let libraryURL = directory.appending(path: "library", directoryHint: .isDirectory)
        let library = PhoneLibrary(directory: libraryURL)
        try library.keep(kind: .dictation, title: "Dictation", text: "Friday.", date: now,
            audio: CapturedAudio(url: source, byteLength: samples.count, sha256: "", durationMs: 1000))
        try FileManager.default.removeItem(at: source)
        let reopened = PhoneLibrary(directory: libraryURL)
        let note = try #require(reopened.notes.first)
        let wave = try #require(reopened.audioURL(note))
        let decoded = try AVAudioFile(forReading: wave)
        #expect(decoded.length == 16000)
        #expect(decoded.fileFormat.sampleRate == 16000)
        #expect(Data(try Data(contentsOf: wave).dropFirst(44)) == samples)
        #expect(note.text == "Friday.")
        try reopened.delete(note)
        #expect(!FileManager.default.fileExists(atPath: wave.path))
        #expect(PhoneLibrary(directory: libraryURL).notes.isEmpty)
    }

    @Test("Recorded calls ring the owner first and keep international numbers intact")
    func callWireContract() throws {
        let account = CallAccount(accountSID: "AC" + String(repeating: "0", count: 32), authToken: "test-only",
            fromNumber: "+1 (202) 555-0100", ownNumber: "+44 20 7946 0123")
        let fields = try CallService.callParameters(account: account, target: "+1 202 555 0123")
        let body = try CallService.form(fields)
        let components = try #require(URLComponents(string: "https://example.test/?" + String(decoding: body, as: UTF8.self)))
        let values = Dictionary(try #require(components.queryItems).map { ($0.name, $0.value ?? "") }, uniquingKeysWith: { _, last in last })
        #expect(values["To"] == "+442079460123")
        #expect(values["From"] == "+12025550100")
        let xml = try #require(values["Twiml"])
        let parser = XMLParser(data: Data(xml.utf8))
        let receiver = DialReceiver()
        parser.delegate = receiver
        #expect(parser.parse())
        #expect(receiver.record == "record-from-answer-dual")
        #expect(receiver.number == "+12025550123")
        #expect(throws: CallServiceError.self) { try CallService.phoneNumber("+12025550123</Number><Hangup/>") }
        #expect(throws: CallServiceError.self) { try CallService.phoneNumber("2025550123") }
    }
}

private final class DialReceiver: NSObject, XMLParserDelegate {
    var record: String?
    var number = ""
    private var inNumber = false
    func parser(_ parser: XMLParser, didStartElement elementName: String, namespaceURI: String?, qualifiedName qName: String?, attributes: [String: String]) {
        if elementName == "Dial" { record = attributes["record"] }
        inNumber = elementName == "Number"
    }
    func parser(_ parser: XMLParser, foundCharacters string: String) { if inNumber { number += string } }
    func parser(_ parser: XMLParser, didEndElement elementName: String, namespaceURI: String?, qualifiedName qName: String?) {
        if elementName == "Number" { inNumber = false }
    }
}

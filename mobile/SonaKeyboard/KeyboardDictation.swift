import Foundation

/// Dictation the keyboard runs itself, reduced to what the person and the microphone did,
/// with no web view, recognizer or clock of its own.
///
/// Separate from the capture so the sequences that went wrong can be tested: the web
/// microphone and Apple's recognizer never run in a unit test.
struct KeyboardDictation {
    enum Phase: Equatable { case idle, starting, listening, finishing }

    enum Event: Equatable {
        /// Dictate was tapped, or held long enough to talk.
        case start
        /// Finish was tapped, or a hold was let go.
        case stop
        case captureStarted
        /// The web view could not open the microphone, or did not say it had in time.
        case captureRefused
        /// The microphone stopped on its own after it started.
        case captureEnded
        case heard(String, final: Bool)
        case recognitionFailed
        /// No recognizer for the language runs on this iPhone; the app has none either.
        case recognizerUnavailable
        /// The recognizer gave no final result in time after the audio ended.
        case finishTimedOut
        /// The keyboard went away; its field may not be the one it comes back to.
        case hidden
    }

    enum Effect: Equatable {
        case beginCapture
        /// Stops the microphone and lets the recognizer finish what it has.
        case endCapture
        /// Stops the microphone and the recognizer and keeps nothing.
        case abandon
        case insert(String)
        /// The microphone is not available to the keyboard; dictate through the app.
        case fallBack
    }

    private(set) var phase: Phase = .idle
    /// The words heard so far in the running dictation.
    private(set) var heard = ""

    mutating func handle(_ event: Event) -> [Effect] {
        switch (phase, event) {
        case (.idle, .start):
            phase = .starting
            heard = ""
            return [.beginCapture]
        case (.starting, .captureStarted):
            phase = .listening
            return []
        case (.starting, .stop), (.starting, .hidden), (.listening, .hidden), (.finishing, .hidden),
             (.starting, .recognizerUnavailable):
            return reset([.abandon])
        case (.starting, .captureRefused), (.starting, .recognitionFailed):
            return reset([.abandon, .fallBack])
        case (.listening, .stop), (.listening, .captureEnded), (.listening, .captureRefused):
            phase = .finishing
            return [.endCapture]
        case let (.listening, .heard(text, final)), let (.finishing, .heard(text, final)):
            heard = text
            return final ? reset([.endCapture] + inserting(text)) : []
        case (.listening, .recognitionFailed) where inserting(heard).isEmpty:
            return reset([.abandon, .fallBack])
        case (.listening, .recognitionFailed), (.finishing, .recognitionFailed), (.finishing, .finishTimedOut):
            return reset([.abandon] + inserting(heard))
        default:
            return []
        }
    }

    private mutating func reset(_ effects: [Effect]) -> [Effect] {
        phase = .idle
        heard = ""
        return effects
    }

    private func inserting(_ text: String) -> [Effect] {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? [] : [.insert(text)]
    }

    /// What a finished dictation puts in the field: the phone's cleanup, then the Mac
    /// profile's replacements and vocabulary. Nil when cleanup leaves nothing, as after a
    /// lone "um": a style given no words writes its own instructions back.
    static func text(of heard: String, profile: DictationProfile, language: String) -> String? {
        let text = profile.apply(to: DictationCleanup.clean(heard, language: language)).trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? nil : text
    }

    enum Landing: Equatable {
        case write
        /// Waits on the panel for Insert.
        case review
    }

    /// What the keyboard knows of the field a dictation is for. Some hosts never name their
    /// fields, so without ids the field counts as the same one until something says
    /// otherwise: the keyboard hid or showed, the text before the cursor changed, or the
    /// field asked for different keys.
    struct FieldMark: Equatable, Codable {
        var id: UUID?
        /// Counts the keyboard hiding and showing.
        var shown: Int
        var before: String
        var traits: String
    }

    /// Where a finished dictation goes: typed into the field it was heard for, or held for
    /// review when review is on or the keyboard is in another field by now, as after a
    /// style that finished late.
    static func landing(heardFor field: FieldMark, now current: FieldMark, reviews: Bool) -> Landing {
        if reviews { return .review }
        if let id = field.id, let now = current.id { return id == now ? .write : .review }
        let same = field.shown == current.shown && field.before == current.before && field.traits == current.traits
        return same ? .write : .review
    }

    /// A dictation waiting on review: its text, the field it was heard for and when.
    struct KeptReview: Equatable, Codable {
        var text: String
        var field: FieldMark
        var date: Date

        /// Another dictation joins this one only from the same field and while this one is
        /// recent; otherwise it is shown on its own and this one waits behind the button.
        func joins(_ next: KeptReview) -> Bool {
            next.date.timeIntervalSince(date) < 5 * 60
                && KeyboardDictation.landing(heardFor: field, now: next.field, reviews: false) == .write
        }

        /// Both texts, this one first, a paragraph apart, under the newer one's field and time.
        func joined(with next: KeptReview) -> KeptReview {
            KeptReview(text: text + "\n\n" + next.text, field: next.field, date: next.date)
        }
    }

    /// Reviews kept across the keyboard being ended: in a file only the keyboard reads,
    /// locked while the phone is, and forgotten after a day. The dictations themselves
    /// also wait in the Notes queue for seven days.
    struct KeptReviewStore {
        static let lifetime: TimeInterval = 24 * 60 * 60
        let url: URL

        static func shared() -> Self? {
            guard let folder = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else { return nil }
            try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            return Self(url: folder.appendingPathComponent("kept-reviews.json"))
        }

        func load(at now: Date) -> [KeptReview] {
            guard let data = try? Data(contentsOf: url),
                  let reviews = try? JSONDecoder().decode([KeptReview].self, from: data) else { return [] }
            return reviews.filter { now.timeIntervalSince($0.date) < Self.lifetime }
        }

        func save(_ reviews: [KeptReview]) {
            guard !reviews.isEmpty, let data = try? JSONEncoder().encode(reviews) else {
                try? FileManager.default.removeItem(at: url)
                return
            }
            try? data.write(to: url, options: [.atomic, .completeFileProtection])
        }
    }
}

/// Where Dictate goes. After the keyboard could not listen, Dictate goes through the app
/// for ten minutes, across keyboard appearances, so later dictations do not wait out
/// another refusal first.
struct KeyboardRoute {
    enum Choice: Equatable {
        case keyboard
        /// The app's warm session listens.
        case appSession
        /// Dictate opens the app.
        case openApp
    }

    static let refusedKey = "sona.keyboard.refusedUntil"
    static let refusalLife: TimeInterval = 10 * 60
    let defaults: UserDefaults

    /// Keeps a fallback the dictation asked for, so Dictate goes through the app from the
    /// next tap on, in this keyboard and the next.
    func record(_ effects: [KeyboardDictation.Effect], at now: Date) {
        guard effects.contains(.fallBack) else { return }
        defaults.set(now.addingTimeInterval(Self.refusalLife), forKey: Self.refusedKey)
    }

    func choice(at now: Date, warm: Bool) -> Choice {
        guard let until = defaults.object(forKey: Self.refusedKey) as? Date, until > now else { return .keyboard }
        return warm ? .appSession : .openApp
    }
}

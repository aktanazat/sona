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

    /// What the keyboard knows of the field a dictation is for. A host's field id is not
    /// enough: some hosts name no fields, and Messages gives its message box the same id in
    /// every conversation. So the field counts as the same one only while nothing says
    /// otherwise: the keyboard did not hide or show, the text before the cursor did not
    /// change, and the field asks for the same keys.
    struct FieldMark: Equatable, Codable {
        var id: UUID?
        /// Counts the keyboard hiding and showing.
        var shown: Int
        var before: String
        var traits: String
    }

    /// Where a finished dictation goes: typed into the field it was heard for, or held for
    /// review when review is on or the keyboard may be in another field by now, as after a
    /// style that finished late.
    static func landing(heardFor field: FieldMark, now current: FieldMark, reviews: Bool) -> Landing {
        if reviews { return .review }
        if let id = field.id, let now = current.id, id != now { return .review }
        let same = field.shown == current.shown && field.before == current.before && field.traits == current.traits
        return same ? .write : .review
    }

    /// A dictation waiting on review: its text, the field it was heard for and when, and
    /// whether it came from the app, whose drafts the Notes queue does not hold.
    struct KeptReview: Equatable, Codable {
        var id = UUID()
        var text: String
        var field: FieldMark
        var date: Date
        var fromApp = false

        /// Another dictation joins this one only from the same field and while this one is
        /// recent; otherwise it is shown on its own and this one waits behind the button.
        func joins(_ next: KeptReview) -> Bool {
            next.date.timeIntervalSince(date) < 5 * 60
                && KeyboardDictation.landing(heardFor: field, now: next.field, reviews: false) == .write
        }

        /// Both texts, this one first, a paragraph apart, under the newer one's field and time.
        func joined(with next: KeptReview) -> KeptReview {
            KeptReview(id: id, text: text + "\n\n" + next.text, field: next.field, date: next.date, fromApp: fromApp || next.fromApp)
        }

        /// How long it is kept: a day for words the keyboard heard, which the Notes queue
        /// also keeps; seven days, as long as that queue, for the app's drafts.
        var lifetime: TimeInterval { (fromApp ? 7 : 1) * 24 * 60 * 60 }

        init(id: UUID = UUID(), text: String, field: FieldMark, date: Date, fromApp: Bool = false) {
            self.id = id
            self.text = text
            self.field = field
            self.date = date
            self.fromApp = fromApp
        }

        /// Reviews saved before they had an id or a source still load.
        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            id = try values.decodeIfPresent(UUID.self, forKey: .id) ?? UUID()
            text = try values.decode(String.self, forKey: .text)
            field = try values.decode(FieldMark.self, forKey: .field)
            date = try values.decode(Date.self, forKey: .date)
            fromApp = try values.decodeIfPresent(Bool.self, forKey: .fromApp) ?? false
        }
    }

    enum Placement: Equatable {
        case write
        case review
        /// Joins the newest kept review on the review page.
        case join
    }

    /// Where a finished dictation goes. A kept review holds it back only when it would join
    /// that review; otherwise it is typed or reviewed as if nothing were kept.
    static func placing(_ review: KeptReview, newestKept: KeptReview?, now current: FieldMark, reviews: Bool) -> Placement {
        if let newestKept, newestKept.joins(review) { return .join }
        return landing(heardFor: review.field, now: current, reviews: reviews) == .write ? .write : .review
    }

    /// Reviews kept across the keyboard being ended: in a file only the keyboard reads,
    /// locked while the phone is, each forgotten after its lifetime. Every keyboard copy
    /// (one per app) merges with the file before it writes, so none drops another's, and
    /// the file remembers which reviews were inserted or discarded, so none comes back.
    struct KeptReviewStore {
        let url: URL

        /// The file: the kept reviews, and when each ended review was inserted or discarded.
        struct Saved: Equatable, Codable {
            var reviews: [KeptReview] = []
            var done: [UUID: Date] = [:]
        }

        /// Ended reviews are remembered as long as the longest-kept review lives.
        static let doneLifetime: TimeInterval = 7 * 24 * 60 * 60

        static func shared() -> Self? {
            guard let folder = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else { return nil }
            try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            return Self(url: folder.appendingPathComponent("kept-reviews.json"))
        }

        /// The kept reviews still in their lifetime: none when there is no file, nil while
        /// the file is locked with the phone.
        func load(at now: Date) -> [KeptReview]? {
            read(at: now)?.reviews
        }

        /// Merges this keyboard copy's reviews with the file and writes the result: `mine`
        /// replaces reviews with the same ids, `ended` are marked done, reviews the file marks
        /// done stay gone, and the rest of the file is kept. Nil while the file is locked,
        /// when nothing is written and the next sync tries again.
        @discardableResult
        func sync(_ mine: [KeptReview], ended: Set<UUID>, at now: Date) -> Saved? {
            guard var saved = read(at: now) else { return nil }
            for id in ended { saved.done[id] = now }
            /* The same review in memory and in the file: the later copy, as a join made it. */
            var merged: [KeptReview] = []
            for review in saved.reviews where !merged.contains(where: { $0.id == review.id }) {
                merged.append(review)
            }
            for review in mine {
                if let index = merged.firstIndex(where: { $0.id == review.id }) {
                    if review.date >= merged[index].date { merged[index] = review }
                } else {
                    merged.append(review)
                }
            }
            /* Oldest first; reviews of the same moment keep the order they came in. */
            saved.reviews = merged.enumerated()
                .filter { saved.done[$0.element.id] == nil && now.timeIntervalSince($0.element.date) < $0.element.lifetime }
                .sorted { ($0.element.date, $0.offset) < ($1.element.date, $1.offset) }
                .map(\.element)
            guard !saved.reviews.isEmpty || !saved.done.isEmpty else {
                try? FileManager.default.removeItem(at: url)
                return saved
            }
            if let data = try? JSONEncoder().encode(saved) {
                try? data.write(to: url, options: [.atomic, .completeFileProtection])
            }
            return saved
        }

        /// The file as it stands, without expired reviews or old done marks. A file that reads
        /// but does not decode is moved aside as `.corrupt` and the store starts empty; a
        /// plain list of reviews is the file before it remembered ended ones.
        private func read(at now: Date) -> Saved? {
            guard FileManager.default.fileExists(atPath: url.path) else { return Saved() }
            guard let data = try? Data(contentsOf: url) else { return nil }
            let decoder = JSONDecoder()
            var saved: Saved
            if let current = try? decoder.decode(Saved.self, from: data) {
                saved = current
            } else if let older = try? decoder.decode([KeptReview].self, from: data) {
                saved = Saved(reviews: older)
            } else {
                let aside = url.appendingPathExtension("corrupt")
                try? FileManager.default.removeItem(at: aside)
                try? FileManager.default.moveItem(at: url, to: aside)
                return Saved()
            }
            saved.reviews = saved.reviews.filter { now.timeIntervalSince($0.date) < $0.lifetime }
            saved.done = saved.done.filter { now.timeIntervalSince($0.value) < Self.doneLifetime }
            return saved
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

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

    /// Where a finished dictation goes: typed into the field it was heard for, or held for
    /// review when review is on, or the keyboard is in another field by now, as after a
    /// style that finished late, or either field is unknown.
    static func landing(heardFor field: UUID?, now current: UUID?, reviews: Bool) -> Landing {
        guard !reviews, let field, field == current else { return .review }
        return .write
    }

    /// The text on review once another dictation lands while one is showing: both, the
    /// earlier first, a paragraph apart.
    static func reviewing(_ text: String, after showing: String?) -> String {
        guard let showing, !showing.isEmpty else { return text }
        return showing + "\n\n" + text
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

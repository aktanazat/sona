import Foundation
import FoundationModels
import NaturalLanguage
import SwiftUI

/// A command he saved from the keyboard: a name and the instruction it runs on the text.
struct SavedCommand: Codable, Equatable, Identifiable {
    var id = UUID()
    var name: String
    var prompt: String
}

/// What the keyboard asks Apple's on-device model to do with a field's text. The phone's
/// dictation styles go through the same model, so the keyboard needs no account or network.
enum KeyboardAI: Equatable {
    case fix
    case ask(String)
    case translate(Locale.Language)
    case command(SavedCommand)
    case style(DictationProfile.Style)

    var title: Text {
        switch self {
        case .fix: Text("tools.ai.fix")
        case .ask: Text("tools.ai.ask")
        case let .translate(language): Text("tools.ai.translatingTo \(Self.name(of: language))")
        case let .command(command): Text(verbatim: command.name)
        case let .style(style): Text(verbatim: style.name)
        }
    }

    /// Whether the action needs text to work on. A question can stand alone.
    var needsText: Bool {
        if case .ask = self { return false }
        return true
    }

    var instructions: String {
        switch self {
        case .fix:
            """
            You are a careful copy editor. You get text someone typed on a phone and return it corrected, \
            in the language it is written in. Never translate.
            Fix every spelling, grammar, punctuation, accent and capitalization mistake, including:
            - tense: a finished event with a past time word ("yesterday", "last night") takes the simple past \
            ("I went", not "I have gone");
            - agreement ("she doesn't", "people are") and pronoun case ("He and I were", not "Me and him was");
            - words that sound alike ("their", "there", "they're"; "accept", "except");
            - questions keep their question marks.
            Keep the writer's meaning, tone, names, links, emoji, line breaks, abbreviations and casual words \
            (asap, thx, lol, 3pm). Change no correct word and add or remove no ideas.
            Reply with the corrected text only.

            Examples:
            Text in English:
            i has went to teh park yesterday and seen a dog lol
            I went to the park yesterday and saw a dog lol.

            Text in English:
            him and me was late cuz the bus dont come
            He and I were late cuz the bus didn't come.

            Text in Spanish:
            no se si el puede venir manana, tu sabes?
            No sé si él puede venir mañana, ¿tú sabes?
            """
        case .ask:
            """
            You help someone write in a text field on their phone. Do what the request asks. \
            When text from the field is given, it is what the request is about. \
            Return only the text to put in the field: no preamble, no quotes, no notes.
            """
        case let .translate(language):
            """
            Translate the text into \(Self.name(of: language, in: Locale(identifier: "en"))). \
            Keep the meaning, tone, names, links, emoji and line breaks. Return only the translation.
            """
        case let .command(command):
            command.prompt + "\nApply this to the text. Return only the resulting text."
        case let .style(style):
            DictationTone.prompt(of: style) + """

                Return only the rewritten text. Never write placeholders in square brackets such as [Name]. \
                Add no subject line, greeting, sign-off or name that the transcript does not contain; leave out what it does not say.
                """
        }
    }

    func prompt(for text: String) -> String {
        switch self {
        case .fix:
            /* Naming the language keeps the model from answering in English. */
            "Text in \(Self.writtenLanguage(of: text)):\n\(text)"
        case let .ask(question):
            text.isEmpty ? question : "Request: \(question)\n\nText from the field:\n\(text)"
        case .style:
            /* The Mac's style prompts expect the cleanup envelope PhoneDictation sends. */
            Self.envelope(text)
        case .translate, .command:
            text
        }
    }

    /// Corrections and translations take the most likely answer, so the same text gets the same fix.
    private var options: GenerationOptions {
        switch self {
        case .fix, .translate: GenerationOptions(samplingMode: .greedy)
        case .ask, .command, .style: GenerationOptions()
        }
    }

    private static func writtenLanguage(of text: String) -> String {
        let recognizer = NLLanguageRecognizer()
        recognizer.processString(text)
        return recognizer.dominantLanguage.flatMap { Locale(identifier: "en").localizedString(forIdentifier: $0.rawValue) }
            ?? "the language it is written in"
    }

    static func name(of language: Locale.Language, in locale: Locale = .current) -> String {
        locale.localizedString(forIdentifier: language.minimalIdentifier) ?? language.minimalIdentifier
    }

    private static func envelope(_ text: String) -> String {
        struct Envelope: Encodable {
            let transcript: String
            let language: String
            let target: [String: String] = [:]
            let context: [String: String] = [:]
        }
        let language = Locale.current.language.minimalIdentifier
        let data = (try? JSONEncoder().encode(Envelope(transcript: text, language: language))) ?? Data()
        return String(decoding: data, as: UTF8.self)
    }

    /// Why the model cannot answer on this phone, or nil when it can.
    static var unavailable: LocalizedStringKey? {
        switch SystemLanguageModel.default.availability {
        case .available: nil
        case .unavailable(.appleIntelligenceNotEnabled): "tools.ai.turnOnIntelligence"
        case .unavailable(.modelNotReady): "tools.ai.modelNotReady"
        case .unavailable: "tools.ai.unsupported"
        }
    }

    /// Languages the model writes, his typing languages first.
    static func languages(first preferred: [TypingLanguage]) -> [Locale.Language] {
        let supported = SystemLanguageModel.default.supportedLanguages
        let ordered = preferred.map { Locale.Language(identifier: $0.rawValue) }
        let mine = ordered.filter { language in supported.contains { $0.languageCode == language.languageCode } }
        let rest = supported
            .filter { language in !mine.contains { $0.languageCode == language.languageCode } }
            .sorted { name(of: $0).localizedStandardCompare(name(of: $1)) == .orderedAscending }
        return mine + rest
    }

    /// A session ready for this action. `prewarm()` on it before the tap loads the model early.
    func session() -> LanguageModelSession {
        LanguageModelSession(instructions: instructions)
    }

    /// The answer, handing each longer part of it to `partial` as the model writes it.
    @MainActor
    func run(_ text: String, in session: LanguageModelSession, partial: (String) -> Void) async throws -> String {
        var reply = ""
        for try await snapshot in session.streamResponse(to: prompt(for: text), options: options) {
            reply = snapshot.content
            /* A style answers in the Mac's JSON envelope; only the finished reply can be read. */
            if !reply.hasPrefix("{") { partial(reply.trimmingCharacters(in: .whitespacesAndNewlines)) }
        }
        let answer = DictationCleanup.rewritten(reply)
        if case .style = self { return DictationCleanup.withoutInventions(answer, source: text) }
        return answer
    }
}

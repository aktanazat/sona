import Foundation
import FoundationModels
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
            You correct text typed on a phone. Fix spelling, grammar, punctuation and capitalization. \
            Keep the meaning, language, tone, wording, line breaks, names, links and emoji. \
            Do not add, remove or explain anything. Return only the corrected text.
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
            DictationTone.prompt(of: style) + "\nReturn only the rewritten text."
        }
    }

    func prompt(for text: String) -> String {
        switch self {
        case let .ask(question):
            text.isEmpty ? question : "Request: \(question)\n\nText from the field:\n\(text)"
        case .style:
            /* The Mac's style prompts expect the cleanup envelope PhoneDictation sends. */
            Self.envelope(text)
        case .fix, .translate, .command:
            text
        }
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

    /// One answer, with the same deadline the phone's style rewrite keeps, longer for a question.
    @MainActor
    func run(_ text: String, using rewrite: BoundedRewrite) async -> BoundedRewrite.Outcome {
        let instructions = instructions
        let prompt = prompt(for: text)
        let seconds: Double = if case .ask = self { 30 } else { 20 }
        return await rewrite.run {
            let session = LanguageModelSession(instructions: instructions)
            let reply = try await session.respond(to: prompt).content
            return DictationCleanup.rewritten(reply)
        } waitForDeadline: {
            try await Task.sleep(for: .seconds(seconds))
        }
    }
}

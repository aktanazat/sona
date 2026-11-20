import SwiftUI
import UIKit

/// The field text an action works on: the selection when there is one, otherwise all the
/// text the field shows the keyboard around the cursor.
struct FieldTarget: Equatable {
    let text: String
    let selected: Bool
}

/// Reading and rewriting the host field through the document proxy, which only deletes
/// before the cursor, moves it and inserts.
@MainActor
enum FieldText {
    static func target(in proxy: UITextDocumentProxy) -> FieldTarget {
        if let selection = proxy.selectedText, !selection.isEmpty { return FieldTarget(text: selection, selected: true) }
        return FieldTarget(text: (proxy.documentContextBeforeInput ?? "") + (proxy.documentContextAfterInput ?? ""), selected: false)
    }

    /// Puts `text` where `target` was. False when the field changed since it was read.
    static func replace(_ target: FieldTarget, with text: String, in proxy: UITextDocumentProxy) -> Bool {
        guard self.target(in: proxy) == target else { return false }
        if !target.selected {
            let after = proxy.documentContextAfterInput ?? ""
            if !after.isEmpty { proxy.adjustTextPosition(byCharacterOffset: after.count) }
            for _ in 0..<target.text.count { proxy.deleteBackward() }
        }
        proxy.insertText(text)
        return true
    }

    /// Takes back a replacement while the cursor still sits right after it.
    static func undo(_ change: Change, in proxy: UITextDocumentProxy) -> Bool {
        guard (proxy.documentContextBeforeInput ?? "").hasSuffix(change.replacement) else { return false }
        for _ in 0..<change.replacement.count { proxy.deleteBackward() }
        proxy.insertText(change.original)
        return true
    }

    struct Change: Equatable {
        let original: String
        let replacement: String
    }
}

/// The keyboard's tools: AI on the field's text, case and styles, snippets, and the choices
/// for the next recording. Owned by the keyboard controller, drawn by `ToolsPanel`.
@MainActor
@Observable
final class KeyboardTools {
    enum Tab: Hashable { case ai, snippets, dictation }
    enum Menu: Hashable { case translate, rewrite, commands, changeCase, styles }
    enum Compose: Equatable {
        case ask
        case commandPrompt
        case commandName(prompt: String)
        case snippetKeyword(expansion: String)
    }

    struct Answer: Equatable {
        let action: KeyboardAI?
        let source: FieldTarget?
        let text: String
    }

    enum Page: Equatable {
        case closed
        case browse(Tab, Menu?)
        case compose(Compose)
        case working(KeyboardAI)
        case answer(Answer)
        /// A dictation waiting for Insert while review is on.
        case review(String)
    }

    struct Notice: Equatable {
        let text: LocalizedStringKey
        var undoable = false
        let id = UUID()

        static func == (lhs: Self, rhs: Self) -> Bool { lhs.id == rhs.id }
    }

    private static let commandsKey = "sona.keyboard.commands"
    private static let snippetsKey = "sona.keyboard.snippets"
    private static let reviewKey = "sona.keyboard.review"

    var page = Page.closed
    var lastTab = Tab.ai
    var composeText = ""
    var notice: Notice?
    var lastChange: FieldText.Change?
    var profile = DictationProfile.empty
    var languages: [TypingLanguage] = [.english]
    var commands: [SavedCommand] { didSet { save(commands, Self.commandsKey) } }
    var snippets: [DictationProfile.Snippet] { didSet { save(snippets, Self.snippetsKey); snippetsChanged() } }
    var reviewsDictation: Bool { didSet { defaults.set(reviewsDictation, forKey: Self.reviewKey) } }
    /// One recording's style id: nil keeps the chosen style, empty is no style.
    var recordingStyle: String?
    var recordingLanguage: TypingLanguage?

    @ObservationIgnored var proxy: () -> UITextDocumentProxy? = { nil }
    @ObservationIgnored var canCopy: () -> Bool = { false }
    @ObservationIgnored var snippetsChanged: () -> Void = {}
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let rewrite = BoundedRewrite()
    @ObservationIgnored private var work: Task<Void, Never>?
    @ObservationIgnored private var noticeTimer: Task<Void, Never>?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        commands = Self.load([SavedCommand].self, Self.commandsKey, from: defaults) ?? []
        snippets = Self.load([DictationProfile.Snippet].self, Self.snippetsKey, from: defaults) ?? []
        reviewsDictation = defaults.bool(forKey: Self.reviewKey)
    }

    var isOpen: Bool { page != .closed }
    var composing: Compose? {
        if case let .compose(purpose) = page { return purpose }
        return nil
    }

    /// The style a recording started now gets.
    var effectiveStyle: DictationProfile.Style? {
        let id = recordingStyle ?? DictationPreferences.style
        return profile.styles.first { $0.id == id }
    }

    // MARK: Navigation

    func toggle() {
        cancelWork()
        page = isOpen ? .closed : .browse(lastTab, nil)
    }

    func show(_ tab: Tab, _ menu: Menu? = nil) {
        lastTab = tab
        page = .browse(tab, menu)
    }

    func back() {
        cancelWork()
        switch page {
        case let .browse(tab, .some):
            page = .browse(tab, nil)
        case .compose(.commandPrompt), .compose(.commandName):
            page = .browse(.ai, .commands)
        case .compose(.snippetKeyword):
            page = .browse(.snippets, nil)
        case .answer, .working, .compose:
            page = .browse(.ai, nil)
        case .review, .closed, .browse:
            page = .closed
        }
    }

    // MARK: Typing into the tools' own field

    func begin(_ purpose: Compose) {
        composeText = ""
        page = .compose(purpose)
    }

    /// A key typed while a tool asks for words. False for keys the keyboard handles itself.
    func type(_ key: Key, face: (String) -> String) -> Bool {
        guard composing != nil else { return false }
        switch key {
        case let .character(text): composeText += face(text)
        case .space: composeText += " "
        case .delete: if !composeText.isEmpty { composeText.removeLast() }
        case .newLine: submit()
        case .shift, .layer, .nextKeyboard, .language: return false
        }
        return true
    }

    func type(word: String) {
        composeText += (composeText.isEmpty || composeText.hasSuffix(" ") ? "" : " ") + word
    }

    func submit() {
        let words = composeText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let purpose = composing, !words.isEmpty else { return }
        composeText = ""
        switch purpose {
        case .ask:
            run(.ask(words))
        case .commandPrompt:
            begin(.commandName(prompt: words))
        case let .commandName(prompt):
            commands.append(SavedCommand(name: words, prompt: prompt))
            show(.ai, .commands)
            say("tools.commandSaved")
        case let .snippetKeyword(expansion):
            snippets.removeAll { $0.trigger.lowercased() == words.lowercased() }
            snippets.append(DictationProfile.Snippet(trigger: words, expansion: expansion))
            show(.snippets)
            say("tools.snippetSaved \(words)")
        }
    }

    // MARK: Field actions

    func run(_ action: KeyboardAI) {
        guard let proxy = proxy() else { return }
        if let reason = KeyboardAI.unavailable { return say(reason) }
        let target = FieldText.target(in: proxy)
        let source: FieldTarget? = target.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : target
        guard source != nil || !action.needsText else { return say("tools.noText") }
        cancelWork()
        page = .working(action)
        work = Task { [weak self] in
            guard let self else { return }
            let outcome = await action.run(source?.text ?? "", using: rewrite)
            guard !Task.isCancelled else { return }
            switch outcome {
            case let .rewritten(text):
                page = .answer(Answer(action: action, source: source, text: text))
            case .fallback:
                page = .browse(.ai, nil)
                say("tools.ai.failed")
            case .cancelled:
                break
            }
        }
    }

    func cancelWork() {
        work?.cancel()
        work = nil
        rewrite.cancel()
    }

    /// Case and styles change the text at once, with Undo on the notice.
    func transform(_ change: (String) -> String) {
        guard let proxy = proxy() else { return }
        let target = FieldText.target(in: proxy)
        guard !target.text.isEmpty else { return say("tools.noText") }
        let replacement = change(target.text)
        guard replacement != target.text, FieldText.replace(target, with: replacement, in: proxy) else { return }
        changed(FieldText.Change(original: target.text, replacement: replacement), "tools.changed")
    }

    func replace(with answer: Answer) {
        guard let proxy = proxy(), let source = answer.source else { return }
        guard FieldText.replace(source, with: answer.text, in: proxy) else {
            return say("tools.textChanged")
        }
        page = .closed
        changed(FieldText.Change(original: source.text, replacement: answer.text), "tools.replaced")
    }

    func insert(_ text: String) {
        guard let proxy = proxy() else { return }
        proxy.insertText(TypingSession.joiningSpace(before: proxy.documentContextBeforeInput ?? "") + text)
        page = .closed
    }

    func copy(_ text: String) {
        UIPasteboard.general.string = text
        say("tools.copied")
    }

    /// Puts back the text the last case change, style or AI replacement took.
    func undo() {
        guard let proxy = proxy(), let change = lastChange else { return }
        lastChange = nil
        notice = nil
        if !FieldText.undo(change, in: proxy) { say("tools.textChanged") }
    }

    private func changed(_ change: FieldText.Change, _ text: LocalizedStringKey) {
        lastChange = change
        say(text, undoable: true)
    }

    // MARK: Snippets

    /// Every snippet the picker offers: his keyboard snippets, then the Mac's.
    var allSnippets: [DictationProfile.Snippet] {
        snippets + profile.snippets.filter { mac in !snippets.contains { $0.trigger.lowercased() == mac.trigger.lowercased() } }
    }

    func saveSelection() {
        guard let selection = proxy()?.selectedText, !selection.isEmpty else { return say("tools.selectFirst") }
        begin(.snippetKeyword(expansion: selection))
    }

    func delete(_ snippet: DictationProfile.Snippet) {
        snippets.removeAll { $0 == snippet }
    }

    func delete(_ command: SavedCommand) {
        commands.removeAll { $0.id == command.id }
    }

    // MARK: Dictation

    /// Dictated text on its way into the field. `styled` is true when the app already
    /// rewrote it; otherwise the recording's style is applied here. With review on it
    /// waits on the panel for Insert. The recording's choices end with it.
    func deliver(_ text: String, styled: Bool) {
        let style = styled ? nil : effectiveStyle
        recordingStyle = nil
        recordingLanguage = nil
        guard let style, KeyboardAI.unavailable == nil else { return land(text) }
        cancelWork()
        page = .working(.style(style))
        work = Task { [weak self] in
            guard let self else { return }
            let outcome = await KeyboardAI.style(style).run(text, using: rewrite)
            guard !Task.isCancelled else { return }
            if case .cancelled = outcome { return }
            if case .fallback = outcome { say("dictation.styleFailed") }
            if case let .rewritten(styledText) = outcome { land(styledText) } else { land(text) }
        }
    }

    private func land(_ text: String) {
        if reviewsDictation {
            page = .review(text)
        } else {
            if case .working = page { page = .closed }
            insert(text)
        }
    }

    // MARK: Notices and storage

    func say(_ text: LocalizedStringKey, undoable: Bool = false) {
        notice = Notice(text: text, undoable: undoable)
        noticeTimer?.cancel()
        noticeTimer = Task { [weak self] in
            try? await Task.sleep(for: .seconds(undoable ? 4 : 2))
            guard !Task.isCancelled else { return }
            self?.notice = nil
        }
    }

    private func save(_ value: some Encodable, _ key: String) {
        defaults.set(try? JSONEncoder().encode(value), forKey: key)
    }

    private static func load<T: Decodable>(_ type: T.Type, _ key: String, from defaults: UserDefaults) -> T? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(type, from: $0) }
    }
}

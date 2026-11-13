import Foundation

/// The written forms of the Mac vocabulary, which the keyboard must never correct away.
/// Only those words reach the shared container: no spoken forms, replacements or snippets.
struct KeyboardVocabularyStore {
    static let fileName = "keyboard-vocabulary.json"

    let directory: URL

    static func shared() throws -> Self {
        Self(directory: try KeyboardDraftStore.shared().directory)
    }

    func save(_ profile: DictationProfile) throws {
        let words = Set(profile.vocabulary.map(\.written)).filter { !$0.isEmpty }.sorted()
        try JSONEncoder().encode(words).write(
            to: directory.appendingPathComponent(Self.fileName),
            options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
        )
    }

    func load() throws -> [String] {
        do {
            let bytes = try Data(contentsOf: directory.appendingPathComponent(Self.fileName))
            return try JSONDecoder().decode([String].self, from: bytes)
        } catch CocoaError.fileReadNoSuchFile {
            return []
        }
    }
}

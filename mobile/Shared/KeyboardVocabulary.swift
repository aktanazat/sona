import Foundation

/// The Mac's dictation profile as the keyboard needs it: the written forms of the
/// vocabulary it must never correct away, and the replacements, vocabulary and snippets
/// it applies to what it hears, as dictation in the app does.
struct KeyboardVocabularyStore {
    static let fileName = "keyboard-profile.json"

    let directory: URL

    static func shared() throws -> Self {
        Self(directory: try KeyboardDraftStore.shared().directory)
    }

    func save(_ profile: DictationProfile) throws {
        try JSONEncoder().encode(profile).write(
            to: directory.appendingPathComponent(Self.fileName),
            options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
        )
    }

    func profile() throws -> DictationProfile {
        do {
            let bytes = try Data(contentsOf: directory.appendingPathComponent(Self.fileName))
            return try JSONDecoder().decode(DictationProfile.self, from: bytes)
        } catch CocoaError.fileReadNoSuchFile {
            return .empty
        }
    }

    func load() throws -> [String] {
        Set(try profile().vocabulary.map(\.written)).filter { !$0.isEmpty }.sorted()
    }
}

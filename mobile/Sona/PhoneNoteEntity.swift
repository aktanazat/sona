import AppIntents
import Combine
import CoreSpotlight
import Foundation

/// A dictation, note or recording in the phone's library, as Spotlight, Siri and
/// Shortcuts see it.
struct PhoneNoteEntity: IndexedEntity {
    static let typeDisplayRepresentation = TypeDisplayRepresentation(
        name: "Sona note", numericFormat: "\(placeholder: .int) Sona notes"
    )
    static let defaultQuery = PhoneNoteQuery()

    let id: UUID
    @Property(title: "Title", indexingKey: \.title)
    var title: String
    @Property(title: "Text", indexingKey: \.textContent)
    var text: String
    @Property(title: "Date", indexingKey: \.contentCreationDate)
    var date: Date
    let kind: PhoneNote.Kind

    init(_ note: PhoneNote) {
        id = note.id
        kind = note.kind
        title = note.title.isEmpty ? String(note.text.prefix(80)) : note.title
        text = note.text
        date = note.date
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(
            title: "\(title)",
            subtitle: "\(date.formatted(date: .abbreviated, time: .shortened))",
            image: .init(systemName: PhoneNoteEntity.symbol(kind))
        )
    }

    static func symbol(_ kind: PhoneNote.Kind) -> String {
        switch kind {
        case .note: return "text.alignleft"
        case .dictation: return "mic"
        case .meeting: return "waveform"
        }
    }
}

struct PhoneNoteQuery: EntityStringQuery {
    @Dependency private var library: PhoneLibrary

    @MainActor
    func entities(for identifiers: [UUID]) async throws -> [PhoneNoteEntity] {
        let wanted = Set(identifiers)
        return library.notes.filter { wanted.contains($0.id) }.map(PhoneNoteEntity.init)
    }

    @MainActor
    func entities(matching string: String) async throws -> [PhoneNoteEntity] {
        PhoneNoteQuery.matching(string, in: library.notes).map(PhoneNoteEntity.init)
    }

    @MainActor
    func suggestedEntities() async throws -> [PhoneNoteEntity] {
        library.notes.prefix(20).map(PhoneNoteEntity.init)
    }

    /// Every word of `string` has to appear in the note's title or text.
    static func matching(_ string: String, in notes: [PhoneNote]) -> [PhoneNote] {
        let words = string.split(whereSeparator: \.isWhitespace).map(String.init)
        guard !words.isEmpty else { return [] }
        return notes.filter { note in
            words.allSatisfy { note.title.localizedStandardContains($0) || note.text.localizedStandardContains($0) }
        }
    }
}

/// Opens one note from Spotlight, Siri or a shortcut.
struct OpenPhoneNoteIntent: OpenIntent {
    static let title: LocalizedStringResource = "Open Sona note"

    @Parameter(title: "Note")
    var target: PhoneNoteEntity

    @Dependency private var model: AppModel

    @MainActor
    func perform() async throws -> some IntentResult {
        model.open(note: target.id)
        return .result()
    }
}

/// "Search Sona": opens the library searching for what was said.
struct SearchPhoneNotesIntent: ShowInAppSearchResultsIntent {
    static let title: LocalizedStringResource = "Search Sona"
    static let searchScopes: [StringSearchScope] = [.general]

    @Parameter(title: "Search")
    var criteria: StringSearchCriteria

    @Dependency private var model: AppModel

    @MainActor
    func perform() async throws -> some IntentResult {
        model.search(library: criteria.term)
        return .result()
    }
}

/// Keeps Spotlight's copy of the library equal to the library.
@MainActor
final class PhoneNoteSpotlight {
    private var indexed: [UUID: PhoneNote] = [:]
    private var watch: AnyCancellable?

    init(library: PhoneLibrary) {
        watch = library.$notes.sink { [weak self] notes in self?.reflect(notes) }
    }

    private func reflect(_ notes: [PhoneNote]) {
        let next = Dictionary(uniqueKeysWithValues: notes.map { ($0.id, $0) })
        let changed = notes.filter { indexed[$0.id] != $0 }.map(PhoneNoteEntity.init)
        let removed = indexed.keys.filter { next[$0] == nil }
        indexed = next
        Task {
            let index = CSSearchableIndex.default()
            if !removed.isEmpty { try? await index.deleteAppEntities(identifiedBy: removed, ofType: PhoneNoteEntity.self) }
            if !changed.isEmpty { try? await index.indexAppEntities(changed) }
        }
    }
}

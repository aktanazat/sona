import AppIntents
import Combine
import CoreSpotlight
import Foundation
import UIKit

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

/// Keeps iPhone search's copy of the library equal to the searchable part of it. The
/// index is readable only while the phone is unlocked.
@MainActor
final class PhoneNoteSpotlight {
    private static let enabledKey = "sona.spotlight.enabled"

    static var isEnabled: Bool {
        UserDefaults.standard.object(forKey: enabledKey) as? Bool ?? true
    }

    private let index = CSSearchableIndex(name: "sona.notes", protectionClass: .complete)
    private let library: PhoneLibrary
    private var book = SearchIndexBook()
    private var watch: AnyCancellable?
    private var unlock: AnyCancellable?
    /// The write in flight; the next one waits for it, so a quick off and on lands in
    /// order.
    private var writing: Task<Void, Never>?

    init(library: PhoneLibrary) {
        self.library = library
        watch = library.$notes.sink { [weak self] notes in self?.reflect(notes) }
        unlock = NotificationCenter.default
            .publisher(for: UIApplication.protectedDataDidBecomeAvailableNotification)
            .sink { [weak self] _ in
                guard let self else { return }
                reflect(library.notes)
            }
    }

    func setEnabled(_ enabled: Bool) {
        UserDefaults.standard.set(enabled, forKey: Self.enabledKey)
        reflect(library.notes)
    }

    private func reflect(_ notes: [PhoneNote]) {
        let enabled = Self.isEnabled
        let previous = writing
        writing = Task { [weak self] in
            await previous?.value
            await self?.write(notes, enabled: enabled)
        }
    }

    private func write(_ notes: [PhoneNote], enabled: Bool) async {
        guard enabled else {
            if (try? await index.deleteAllSearchableItems()) != nil { book.clear() }
            return
        }
        let plan = book.plan(notes)
        guard !plan.isEmpty else { return }
        do {
            if !plan.removed.isEmpty {
                try await index.deleteSearchableItems(withIdentifiers: plan.removed.map(\.uuidString))
            }
            if !plan.changed.isEmpty { try await index.indexSearchableItems(plan.changed.map(Self.item)) }
            book.confirm(plan)
        } catch {
            /* Locked: the protected index is closed. Unlocking sends the same plan again. */
        }
    }

    /// An item of this index, not `indexAppEntities`, so it carries the index's
    /// protection class; the associated entity still opens the note when tapped.
    private static func item(_ note: PhoneNote) -> CSSearchableItem {
        let entity = PhoneNoteEntity(note)
        let attributes = CSSearchableItemAttributeSet(contentType: .text)
        attributes.title = entity.title
        attributes.textContent = entity.text
        attributes.contentCreationDate = entity.date
        attributes.associateAppEntity(entity, priority: 1)
        return CSSearchableItem(uniqueIdentifier: note.id.uuidString, domainIdentifier: "notes", attributeSet: attributes)
    }
}

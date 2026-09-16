import Foundation

struct KeyboardDraft: Codable, Identifiable, Equatable {
    let id: UUID
    let text: String
    let createdAt: Date
}

enum KeyboardDraftError: LocalizedError {
    case unavailable
    case expired
    case changed
    case empty

    var errorDescription: String? {
        let key: String
        switch self {
        case .unavailable: key = "dictation.storageUnavailable"
        case .expired: key = "dictation.draftExpired"
        case .changed: key = "dictation.draftChanged"
        case .empty: key = "dictation.empty"
        }
        return NSLocalizedString(key, comment: "")
    }
}

/// `missing` and `expired` both mean nothing can be inserted, but only `expired` can tell
/// the user why, so the keyboard does not answer a timed-out draft with first-run advice.
enum KeyboardDraftState: Equatable {
    case missing
    case expired
    case waiting(KeyboardDraft)
}

/// The app and keyboard share only the draft the user has approved for insertion.
struct KeyboardDraftStore {
    static let group = "group.com.aktanazat.sona.mobile"
    static let fileName = "keyboard-draft.json"
    static let lifetime: TimeInterval = 600

    let directory: URL

    static func shared() throws -> Self {
        guard let directory = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: group
        ) else { throw KeyboardDraftError.unavailable }
        return Self(directory: directory)
    }

    @discardableResult
    func save(text: String, now: Date = Date()) throws -> KeyboardDraft {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { throw KeyboardDraftError.empty }
        let draft = KeyboardDraft(id: UUID(), text: text, createdAt: now)
        try coordinated { file in
            let bytes = try JSONEncoder().encode(draft)
            try bytes.write(to: file, options: [.atomic, .completeFileProtection])
            /* The exclusion is a hint on a draft the keyboard can already read, so its
             * failure must not be reported as a save that did not happen. */
            var file = file
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try? file.setResourceValues(values)
        }
        return draft
    }

    func load(now: Date = Date()) throws -> KeyboardDraftState {
        try coordinated { try state(of: $0, now: now) }
    }

    /// Check the preview's identity and remove it under the same cross-process lock.
    func take(id: UUID, now: Date = Date()) throws -> String {
        try coordinated { file in
            switch try state(of: file, now: now) {
            case .expired:
                throw KeyboardDraftError.expired
            case .missing:
                throw KeyboardDraftError.changed
            case .waiting(let draft):
                guard draft.id == id else { throw KeyboardDraftError.changed }
                try FileManager.default.removeItem(at: file)
                return draft.text
            }
        }
    }

    func discard(id: UUID) throws {
        _ = try take(id: id)
    }

    private func state(of file: URL, now: Date) throws -> KeyboardDraftState {
        let bytes: Data
        do {
            bytes = try Data(contentsOf: file)
        } catch CocoaError.fileReadNoSuchFile {
            return .missing
        }
        guard let draft = try? JSONDecoder().decode(KeyboardDraft.self, from: bytes) else {
            /* A draft nobody can decode is also a draft nobody can discard, so clear it
             * here instead of answering every refresh with the same decoding failure. */
            try FileManager.default.removeItem(at: file)
            return .missing
        }
        /* Only the upper bound: a clock moved backwards must not destroy approved text. */
        guard now.timeIntervalSince(draft.createdAt) < Self.lifetime else {
            try FileManager.default.removeItem(at: file)
            return .expired
        }
        return .waiting(draft)
    }

    /* The claim is on the container rather than the draft, so create, read, and delete all
     * serialize against each other, and the accessor's URL is the one that gets used. */
    private func coordinated<T>(_ operation: (URL) throws -> T) throws -> T {
        let coordinator = NSFileCoordinator(filePresenter: nil)
        var coordinationError: NSError?
        var result: Result<T, Error>?
        coordinator.coordinate(writingItemAt: directory, options: [], error: &coordinationError) { folder in
            result = Result { try operation(folder.appendingPathComponent(Self.fileName)) }
        }
        if let coordinationError { throw coordinationError }
        guard let result else { throw KeyboardDraftError.unavailable }
        return try result.get()
    }
}

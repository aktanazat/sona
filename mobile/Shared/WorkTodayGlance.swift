import Foundation

/// What the Today widgets show, written into the shared container each time the app
/// reads the Mac's snapshot. A widget cannot open the vault, so it shows this copy and
/// when the Mac wrote it.
struct WorkTodayGlance: Codable, Equatable {
    static let widgetKind = "com.aktanazat.sona.mobile.today"
    /// Opens the Mac's Today in the app. The scheme is `DictationLink.scheme`, which the
    /// widgets do not build.
    static let url = URL(string: "sona://today")!
    static let fileName = "work-today.json"
    static let shownTitles = 3

    var producedAt: Date
    var dueTitles: [String]
    var dueCount: Int
    var triageCount: Int
    var questionCount: Int
    var inboxCount: Int

    /// Asks the user for a decision: an agent waits, or a task waits in Triage.
    var decisions: Int { questionCount + triageCount }

    init(producedAt: Date, dueTitles: [String], dueCount: Int, triageCount: Int, questionCount: Int, inboxCount: Int) {
        self.producedAt = producedAt
        self.dueTitles = dueTitles
        self.dueCount = dueCount
        self.triageCount = triageCount
        self.questionCount = questionCount
        self.inboxCount = inboxCount
    }

    init(_ today: WorkToday) {
        self.init(
            producedAt: today.producedAt,
            dueTitles: today.due.prefix(Self.shownTitles).map(\.title),
            dueCount: today.due.count,
            triageCount: today.triage.count,
            questionCount: today.questions.count,
            inboxCount: today.inbox.count
        )
    }

    private static var file: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: KeyboardDraftStore.group)?
            .appending(path: fileName)
    }

    static func load() -> WorkTodayGlance? {
        guard let file, let bytes = try? Data(contentsOf: file) else { return nil }
        return try? JSONDecoder().decode(WorkTodayGlance.self, from: bytes)
    }

    /// Readable after the first unlock, so a Lock Screen widget can draw it.
    func save() throws {
        guard let file = Self.file else { throw CocoaError(.fileNoSuchFile) }
        try JSONEncoder().encode(self).write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    static func remove() {
        guard let file else { return }
        try? FileManager.default.removeItem(at: file)
    }
}

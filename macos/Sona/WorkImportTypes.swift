import Foundation

/// The tracker an import reads, as the core names it.
enum WorkImportSource: String, Codable, CaseIterable, Identifiable {
    case linear, csv, jira, github, asana, shortcut, trello

    var id: String { rawValue }

    var title: String {
        switch self {
        case .linear: "Linear"
        case .csv: "Spreadsheet (CSV)"
        case .jira: "Jira"
        case .github: "GitHub"
        case .asana: "Asana"
        case .shortcut: "Shortcut"
        case .trello: "Trello"
        }
    }

    /// The export each tracker writes, named the way its own menus name it.
    var exportName: String {
        switch self {
        case .linear: "Linear's CSV export or a saved API answer"
        case .csv: "A CSV file with a header row"
        case .jira: "Jira's CSV export"
        case .github: "The JSON from gh issue list --json"
        case .asana: "Asana's CSV export"
        case .shortcut: "Shortcut's CSV export"
        case .trello: "A Trello board's JSON export"
        }
    }

    /// What its exports can carry beside the tasks.
    var copies: [WorkImportCopyKind] {
        switch self {
        case .linear: WorkImportCopyKind.allCases
        case .csv, .asana, .shortcut: [.labels, .projects]
        case .jira, .github, .trello: [.labels, .projects, .comments]
        }
    }

    var fileExtensions: [String] {
        switch self {
        case .linear: ["csv", "json"]
        case .csv, .jira, .asana, .shortcut: ["csv"]
        case .github, .trello: ["json"]
        }
    }
}

/// What comes over beside the tasks.
struct WorkImportCopy: Codable, Equatable, Hashable {
    var labels = true
    var projects = true
    var milestones = true
    var cycles = true
    var initiatives = true
    var comments = true
    var documents = true
    var views = true
}

enum WorkImportCopyKind: CaseIterable, Identifiable {
    case labels, projects, milestones, cycles, initiatives, comments, documents, views

    var id: Self { self }

    var title: String {
        switch self {
        case .labels: "Labels"
        case .projects: "Projects"
        case .milestones: "Milestones"
        case .cycles: "Cycles"
        case .initiatives: "Initiatives"
        case .comments: "Comments"
        case .documents: "Documents"
        case .views: "Views"
        }
    }

    var keyPath: WritableKeyPath<WorkImportCopy, Bool> {
        switch self {
        case .labels: \.labels
        case .projects: \.projects
        case .milestones: \.milestones
        case .cycles: \.cycles
        case .initiatives: \.initiatives
        case .comments: \.comments
        case .documents: \.documents
        case .views: \.views
        }
    }
}

/// Which tasks come over, by where they stand there.
struct WorkImportInclude: Codable, Equatable, Hashable {
    var active = true
    var stale = true
    var completed = true
}

struct WorkImportOptions: Codable, Equatable, Hashable {
    var copy = WorkImportCopy()
    var include = WorkImportInclude()
    var updateExisting = false
    var keepNumbers = false
    /// None for no label.
    var label: String? = "Migrated"
    var me: [String] = []
}

enum WorkImportState: String, Decodable {
    case active, stale, completed
}

enum WorkImportKind: String, Decodable {
    case task, project, milestone, label, initiative, comment, note, view
}

enum WorkImportAction: String, Decodable {
    case made, archived, matched, updated, skipped, leftOut = "left_out"

    var word: String {
        switch self {
        case .made: "New"
        case .archived: "Archived"
        case .matched: "Matched"
        case .updated: "Updated"
        case .skipped: "Already here"
        case .leftOut: "Left out"
        }
    }
}

struct WorkImportRow: Decodable, Identifiable {
    let kind: WorkImportKind
    let title: String
    let sourceRef: String?
    let state: WorkImportState?
    let action: WorkImportAction
    let reason: String?

    /// Rows carry no id of their own; the core lists them in a fixed order.
    var id: String { "\(kind.rawValue)|\(sourceRef ?? "")|\(title)" }
}

struct WorkImportTally: Decodable, Equatable {
    let made: Int
    let archived: Int
    let matched: Int
    let updated: Int
    let skipped: Int
    let leftOut: Int

    /// Made new, in the lists or straight into the archive.
    var brought: Int { made + archived }
}

struct WorkImportStates: Decodable, Equatable {
    let active: Int
    let stale: Int
    let completed: Int
}

struct WorkImportTotals: Decodable, Equatable {
    let found: WorkImportStates
    let tasks: WorkImportTally
    let projects: WorkImportTally
    let milestones: WorkImportTally
    let labels: WorkImportTally
    let initiatives: WorkImportTally
    let comments: WorkImportTally
    let notes: WorkImportTally
    let views: WorkImportTally
    let inCycles: Int

    /// "14 tasks, 2 projects, 3 labels": what is new, kinds with none left out.
    var madeSentence: String {
        let parts = [
            (tasks.brought, "task", "tasks"),
            (projects.brought, "project", "projects"),
            (milestones.brought, "milestone", "milestones"),
            (labels.brought, "label", "labels"),
            (initiatives.brought, "initiative", "initiatives"),
            (comments.brought, "comment", "comments"),
            (notes.brought, "note", "notes"),
            (views.brought, "view", "views"),
        ]
        .filter { $0.0 > 0 }
        .map { "\($0.0) \($0.0 == 1 ? $0.1 : $0.2)" }
        return parts.isEmpty ? "Nothing new" : parts.joined(separator: ", ")
    }
}

/// What an import did, or in a preview what it would do.
struct WorkImportReport: Decodable {
    let id: String?
    let source: WorkImportSource
    let name: String
    let label: String?
    let totals: WorkImportTotals
    let rows: [WorkImportRow]
    let notes: [String]
    let numbersKeepable: Bool
}

struct WorkImportRecord: Decodable, Identifiable {
    let id: String
    let source: WorkImportSource
    let name: String
    let totals: WorkImportTotals
    let createdAtMs: Int64
    let revertedAtMs: Int64?

    var date: Date { Date(timeIntervalSince1970: TimeInterval(createdAtMs) / 1000) }
}

struct WorkImportUndone: Decodable {
    let tasks: Int
    let projects: Int
    let notesKept: Int
}

struct WorkImportReverted: Decodable {
    let undone: WorkImportUndone
}

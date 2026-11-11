import Foundation
import Observation

struct WorkTemplateFields: Codable, Equatable {
    var title: String?
    var details: String?
    var status: WorkStatus?
    var priority: Int?
    var owner: WorkOwner?
    var projectId: String?
    var labelIds: [String] = []
    var dueInDays: Int?
    var recurrence: String?
    var subtasks: [String] = []
}

struct WorkTemplate: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var fields: WorkTemplateFields
    var revision: Int64
    var createdAtMs: Int64
    var updatedAtMs: Int64
}

struct WorkReminder: Codable, Equatable {
    var itemId: String
    var remindAtMs: Int64
    var setAtMs: Int64
    var actor: WorkActor
    var agent: String?
    var notifiedAtMs: Int64?
}

@MainActor
@Observable
final class RoutinesStore {
    private(set) var templates: [WorkTemplate] = []
    private(set) var reminders: [String: WorkReminder] = [:]
    private(set) var error: String?

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        core.observe("work-changed") { [weak self] _ in
            Task { await self?.loadReminders() }
        }
    }

    func start() async {
        await loadTemplates()
        await loadReminders()
    }

    func loadTemplates() async {
        do {
            templates = Self.sorted(try await core.request("work_templates"))
        } catch {
            self.error = error.localizedDescription
        }
    }

    func loadReminders() async {
        do {
            let listed: [WorkReminder] = try await core.request("work_reminders", RemindersParams(dueOnly: false))
            reminders = Dictionary(listed.map { ($0.itemId, $0) }, uniquingKeysWith: { _, last in last })
        } catch {
            self.error = error.localizedDescription
        }
    }

    @discardableResult
    func saveTemplate(id: String? = nil, name: String, fields: WorkTemplateFields) async -> WorkTemplate? {
        error = nil
        do {
            let saved: WorkTemplate = try await core.request("work_template_save", TemplateSaveParams(
                operationId: Self.newId(), id: id, name: name, fields: fields))
            templates = Self.sorted(templates.filter { $0.id != saved.id } + [saved])
            return saved
        } catch {
            self.error = error.localizedDescription
            return nil
        }
    }

    func rename(_ template: WorkTemplate, to name: String) async {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed != template.name else { return }
        await saveTemplate(id: template.id, name: trimmed, fields: template.fields)
    }

    func delete(_ template: WorkTemplate) async {
        error = nil
        do {
            try await core.request("work_template_delete", IdParams(operationId: Self.newId(), id: template.id))
            templates.removeAll { $0.id == template.id }
        } catch {
            self.error = error.localizedDescription
        }
    }

    func use(_ template: WorkTemplate, title: String?, dueAtMs: Int64?, status: WorkStatus?) async throws -> [WorkItem] {
        try await core.request("work_template_use", TemplateUseParams(
            operationId: Self.newId(), id: template.id,
            using: TemplateUse(title: title, dueAtMs: dueAtMs, status: status)))
    }

    func detail(_ id: String) async throws -> WorkItemDetail {
        try await core.request("work_item", ItemParams(id: id))
    }

    func setReminder(on itemId: String, at moment: Int64?) async {
        error = nil
        do {
            let saved: WorkReminder? = try await core.request("work_reminder_set", ReminderSetParams(
                operationId: Self.newId(), id: itemId, atMs: moment))
            reminders[itemId] = saved
        } catch {
            self.error = error.localizedDescription
        }
    }

    private static func sorted(_ templates: [WorkTemplate]) -> [WorkTemplate] {
        templates.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    private static func newId() -> String { UUID().uuidString.lowercased() }
}

private struct RemindersParams: Encodable { let dueOnly: Bool }
private struct ItemParams: Encodable { let id: String }
private struct IdParams: Encodable {
    let operationId: String
    let id: String
}
private struct TemplateSaveParams: Encodable {
    let operationId: String
    let id: String?
    let name: String
    let fields: WorkTemplateFields
}
private struct TemplateUse: Encodable {
    let title: String?
    let dueAtMs: Int64?
    let status: WorkStatus?
}
private struct TemplateUseParams: Encodable {
    let operationId: String
    let id: String
    let using: TemplateUse
}
private struct ReminderSetParams: Encodable {
    let operationId: String
    let id: String
    let atMs: Int64?
}

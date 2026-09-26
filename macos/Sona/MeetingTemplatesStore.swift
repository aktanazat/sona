import Foundation
import Observation

/// Every fenced write carries the revision it was decided against. The inner
/// body is snake_case; only the parameter around it is camelCase.
private struct MeetingTemplatesEnvelope<Body: Encodable>: Encodable {
    let request: Body
}

/// The person's own templates: the list, and the two writes to it.
///
/// One object for every surface that names a template — the settings section
/// that edits them, the picker on a meeting, the pickers on a series, the
/// preview of a meeting about to start — so a name is looked up in one place
/// and a save in one window is what the next read in another shows. Every
/// write carries the list's revision; a write the core fences out reloads the
/// list and says so, and the person presses again.
@MainActor @Observable final class MeetingTemplatesStore {
    private let core: Core

    /// Sorted by name, as the core keeps them.
    private(set) var templates: [MeetingCustomTemplate] = []
    /// The fence the next write carries.
    private(set) var revision = 0
    /// True once the first read has answered, well or badly.
    private(set) var read = false
    /// Why the list could not be read. Nil once a read lands.
    private(set) var loadFailure: String?
    private(set) var saving = false
    /// What the last save or delete could not do, for the editor to show.
    private(set) var note: String?

    init(core: Core) {
        self.core = core
    }

    func load() async {
        do {
            let list: MeetingCustomTemplates = try await core.request("meeting_custom_templates_list")
            accept(list)
            loadFailure = nil
        } catch {
            loadFailure = Self.sentence(error, "Sona could not read your templates.")
        }
        read = true
    }

    /// The read a surface that only needs names asks for: once, and again
    /// only while the last read failed. A list that never loads costs those
    /// surfaces the custom names, never the built-ins.
    func loadIfNeeded() async {
        guard !read || loadFailure != nil else { return }
        await load()
    }

    func template(_ templateId: String) -> MeetingCustomTemplate? {
        templates.first { $0.templateId == templateId }
    }

    /// The name a picker prints for a custom id: the template's own name;
    /// "Your template" while the list has not been read, so a choice is never
    /// drawn as the built-in it is not; nil once the list was read and holds
    /// no such template.
    func name(_ templateId: String) -> String? {
        if let name = template(templateId)?.name { return name }
        return read && loadFailure == nil ? nil : "Your template"
    }

    /// The words a picker prints for a choice: the built-in's label, the
    /// custom template's name, or `none` for no choice and for a custom id
    /// the read list does not hold.
    func label<BuiltIn: Hashable>(
        for choice: MeetingTemplateChoice<BuiltIn>?, builtIn: (BuiltIn) -> String, none: String
    ) -> String {
        switch choice {
        case nil: none
        case let .builtIn(template)?: builtIn(template)
        case let .custom(templateId)?: name(templateId) ?? none
        }
    }

    /// True while the list holds as many templates as the core keeps.
    var atLimit: Bool {
        templates.count >= MeetingTemplateLimits.maxTemplates
    }

    /// The editor's last failure is its own: opening it again starts clean.
    func clearNote() {
        note = nil
    }

    /// Saves a draft as a new template, or over `templateId`. Answers the id
    /// the template has now, or nil with `note` saying why not. The draft is
    /// checked here first: a limit the core would refuse is said in the
    /// editor's words rather than the core's.
    @discardableResult
    func save(_ draft: MeetingCustomTemplateDraft, replacing templateId: String?) async -> String? {
        guard !saving else { return nil }
        let outgoing = draft.trimmed
        if let problem = outgoing.problem {
            note = problem
            return nil
        }
        saving = true
        defer { saving = false }
        note = nil
        do {
            let result: MeetingCustomTemplateSaveResult = try await core.request(
                "meeting_custom_template_save",
                MeetingTemplatesEnvelope(
                    request: MeetingCustomTemplateSaveRequest(
                        templateId: templateId, draft: outgoing, expectedRevision: revision)))
            accept(result.templates)
            return result.templateId
        } catch {
            await refuse(error, "Sona could not save this template.")
            return nil
        }
    }

    /// Deletes one template. The core clears every series choice that pointed
    /// at it and lets every meeting that used it fall back to the default.
    func delete(_ templateId: String) async -> Bool {
        guard !saving else { return false }
        saving = true
        defer { saving = false }
        note = nil
        do {
            let list: MeetingCustomTemplates = try await core.request(
                "meeting_custom_template_delete",
                MeetingTemplatesEnvelope(
                    request: MeetingCustomTemplateDeleteRequest(templateId: templateId, expectedRevision: revision)))
            accept(list)
            return true
        } catch {
            await refuse(error, "Sona could not delete this template.")
            return false
        }
    }

    private func accept(_ list: MeetingCustomTemplates) {
        templates = list.templates
        revision = list.revision
    }

    /// A refused write, as a sentence. The two refusals with a next step
    /// reload the list first: somebody else moved it, or the template is
    /// already gone, and either way what is on screen is no longer what is
    /// stored.
    private func refuse(_ error: Error, _ fallback: String) async {
        switch (error as? CoreError)?.remote(as: MeetingSettingsCommandError.self) {
        case .staleRevision?:
            await load()
            note = "This list changed in another window. Try again."
        case .notFound?:
            await load()
            note = "This template was deleted in another window."
        case .invalidRequest?:
            note = "Sona could not save this template. Check the name and the sections."
        default:
            note = Self.sentence(error, fallback)
        }
    }

    /// A command error the core names itself, said as a sentence; anything
    /// else as the caller's fallback.
    private static func sentence(_ error: Error, _ fallback: String) -> String {
        if let remote = (error as? CoreError)?.remote(as: MeetingSettingsCommandError.self) {
            return remote.sentence
        }
        return (error as? CoreError)?.errorDescription ?? fallback
    }
}

import Foundation

/// The wire shapes behind custom meeting templates: a template, the list the
/// core keeps them in, what the editor sends, and the limits the editor
/// mirrors so Save refuses a draft before the core does.
///
/// Field names mirror `src-tauri/src` with snake_case turned into camelCase
/// by `Core.decoder`. The fenced request bodies are written by hand in
/// snake_case, as every other meeting write is; the command parameters around
/// them are camelCase, which is how the dispatcher reads them.

// MARK: - Templates

/// One heading in the notes, and what the model writes under it.
struct MeetingTemplateSection: Codable, Equatable, Hashable {
    var title: String
    var instructions: String
}

/// A template as the core stores it.
struct MeetingCustomTemplate: Decodable, Equatable, Identifiable {
    let templateId: String
    let name: String
    /// "What this template is for". Empty when nothing was said.
    let purpose: String
    let sections: [MeetingTemplateSection]
    let createdAtUtcMs: Int64
    let updatedAtUtcMs: Int64

    var id: String { templateId }

    /// "3 sections": the fact a row shows beside the name.
    var sectionsLine: String {
        sections.count == 1 ? "1 section" : "\(sections.count) sections"
    }
}

/// Every template, sorted by name, and the fence every write to the list
/// carries.
struct MeetingCustomTemplates: Decodable {
    let templates: [MeetingCustomTemplate]
    let revision: Int
}

/// What a save answers: the id the template has now, and the whole list.
struct MeetingCustomTemplateSaveResult: Decodable {
    let templateId: String
    let templates: MeetingCustomTemplates
}

// MARK: - The draft and its limits

/// The limits the core enforces, mirrored so the editor can say which one a
/// draft breaks and keep Save off until it does not. The core is the
/// authority: it trims every field and refuses anything outside these. Text
/// limits are UTF-8 bytes, which is what the core counts.
enum MeetingTemplateLimits {
    static let nameBytes = 60
    static let purposeBytes = 300
    static let sectionTitleBytes = 60
    static let sectionInstructionsBytes = 240
    static let maxSections = 8
    static let maxTemplates = 50
}

/// A template without its identity or its dates: what the editor sends.
struct MeetingCustomTemplateDraft: Encodable, Equatable {
    var name: String
    var purpose: String
    var sections: [MeetingTemplateSection]

    init(name: String = "", purpose: String = "", sections: [MeetingTemplateSection] = []) {
        self.name = name
        self.purpose = purpose
        self.sections = sections
    }

    /// An existing template, opened for editing.
    init(_ template: MeetingCustomTemplate) {
        name = template.name
        purpose = template.purpose
        sections = template.sections
    }

    /// Every field trimmed the way the core trims it, so the check here and
    /// the check there read the same text.
    var trimmed: MeetingCustomTemplateDraft {
        MeetingCustomTemplateDraft(
            name: Self.trim(name),
            purpose: Self.trim(purpose),
            sections: sections.map {
                MeetingTemplateSection(title: Self.trim($0.title), instructions: Self.trim($0.instructions))
            })
    }

    /// The first thing wrong with the trimmed draft, as one short sentence,
    /// or nil when the core would accept it. Checked in reading order: the
    /// name, the purpose, then each section top to bottom.
    var problem: String? {
        let draft = trimmed
        if draft.name.isEmpty { return "Give the template a name." }
        if draft.name.contains(where: \.isNewline) { return "Keep the name on one line." }
        if draft.name.utf8.count > MeetingTemplateLimits.nameBytes {
            return "Keep the name under \(MeetingTemplateLimits.nameBytes) characters."
        }
        if draft.purpose.utf8.count > MeetingTemplateLimits.purposeBytes {
            return "Keep what this template is for under \(MeetingTemplateLimits.purposeBytes) characters."
        }
        if draft.sections.isEmpty { return "Add at least one section." }
        if draft.sections.count > MeetingTemplateLimits.maxSections {
            return "A template can have up to \(MeetingTemplateLimits.maxSections) sections."
        }
        var titles = Set<String>()
        for section in draft.sections {
            if section.title.isEmpty { return "Every section needs a title." }
            if section.title.contains(where: \.isNewline) { return "Keep section titles on one line." }
            if section.title.utf8.count > MeetingTemplateLimits.sectionTitleBytes {
                return "Keep section titles under \(MeetingTemplateLimits.sectionTitleBytes) characters."
            }
            if section.instructions.utf8.count > MeetingTemplateLimits.sectionInstructionsBytes {
                return "Keep section instructions under \(MeetingTemplateLimits.sectionInstructionsBytes) characters."
            }
            if !titles.insert(section.title.lowercased()).inserted {
                return "Two sections are called \"\(section.title)\". Give each section its own title."
            }
        }
        return nil
    }

    private static func trim(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// `MeetingCustomTemplateSaveRequest`: a null id creates a template, an id
/// replaces that template.
struct MeetingCustomTemplateSaveRequest: Encodable {
    let templateId: String?
    let draft: MeetingCustomTemplateDraft
    let expectedRevision: Int

    private enum CodingKeys: String, CodingKey {
        case templateId = "template_id"
        case draft
        case expectedRevision = "expected_revision"
    }

    /// The null id is what creates a template, so the key is always present.
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(templateId, forKey: .templateId)
        try container.encode(draft, forKey: .draft)
        try container.encode(expectedRevision, forKey: .expectedRevision)
    }
}

struct MeetingCustomTemplateDeleteRequest: Encodable {
    let templateId: String
    let expectedRevision: Int

    private enum CodingKeys: String, CodingKey {
        case templateId = "template_id"
        case expectedRevision = "expected_revision"
    }
}

// MARK: - What a picker chooses

/// A template as any picker names it: one of the core's built-ins, or one of
/// the person's own by id.
///
/// Generic over the built-in enum because three surfaces spell the built-ins
/// with their own type and the same wire names (`MeetingNotesTemplate`,
/// `MeetingSeriesTemplate`, `OverviewTemplate`), and a custom choice has to
/// sit beside each of them without loosening any of their decoders.
enum MeetingTemplateChoice<BuiltIn: Hashable>: Hashable {
    case builtIn(BuiltIn)
    case custom(String)

    var builtIn: BuiltIn? {
        if case let .builtIn(template) = self { template } else { nil }
    }

    var customTemplateId: String? {
        if case let .custom(templateId) = self { templateId } else { nil }
    }

    /// The choice a stored pair spells: the custom id when there is one, else
    /// the built-in, else nothing.
    init?(builtIn: BuiltIn?, customTemplateId: String?) {
        if let customTemplateId {
            self = .custom(customTemplateId)
        } else if let builtIn {
            self = .builtIn(builtIn)
        } else {
            return nil
        }
    }
}

// MARK: - Notes language

/// Which language the generated notes are written in. `auto` follows the
/// language most of the meeting was spoken in.
enum MeetingNotesLanguage: String, Codable, CaseIterable, Identifiable {
    case auto
    case english

    var id: String { rawValue }

    var label: String {
        switch self {
        case .auto: "Same as the meeting"
        case .english: "English"
        }
    }
}

// MARK: - The two settings writes

/// `change_meeting_notes_template_setting`. The built-in is always sent: it
/// is the fallback the core writes with when the custom template is gone.
struct MeetingNotesTemplateWrite: Encodable {
    let template: MeetingNotesTemplate
    let customTemplateId: String?

    private enum CodingKeys: String, CodingKey {
        case template, customTemplateId
    }

    /// A null id is what hands the default back to the built-in, so the key
    /// is always present.
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(template.rawValue, forKey: .template)
        try container.encode(customTemplateId, forKey: .customTemplateId)
    }
}

/// `change_meeting_notes_language_setting`.
struct MeetingNotesLanguageWrite: Encodable {
    let language: MeetingNotesLanguage
}

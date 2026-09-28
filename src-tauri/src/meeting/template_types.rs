//! Notes templates a person writes for themselves, and the language generated
//! notes are written in.
//!
//! A built-in template ([`MeetingNotesTemplate`]) only asks for a different
//! emphasis. A custom one names the sections the notes are arranged under: its
//! sections become the outline, in the order written, each title exactly as
//! typed. Everything else in the notes — the summary, decisions, action items,
//! questions, risks and the follow-up draft — keeps its place, so a template
//! can reshape the notes but never talk the model out of citing what it writes.
//!
//! Which template a meeting is written with is decided per meeting, then per
//! calendar series, then per folder, then by the app default. A custom choice
//! at any rung names a template by id; a template that has since been deleted
//! is skipped and the next rung decides, which is why every choice below
//! carries the built-in it falls back to.

use super::analytics::MeetingNotesTemplate;
pub use super::types::MeetingTemplateId;
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

pub const MAX_TEMPLATE_NAME_BYTES: usize = 60;
pub const MAX_TEMPLATE_PURPOSE_BYTES: usize = 300;
pub const MAX_TEMPLATE_SECTIONS: usize = 8;
pub const MAX_SECTION_TITLE_BYTES: usize = 60;
pub const MAX_SECTION_INSTRUCTIONS_BYTES: usize = 240;
pub const MAX_CUSTOM_TEMPLATES: usize = 50;

/// How a custom template is named on an artifact revision. Built-in ids never
/// contain a colon, so the two spellings cannot collide.
const CUSTOM_ARTIFACT_TEMPLATE_PREFIX: &str = "custom:";

/// One heading of a custom template and what belongs under it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingTemplateSection {
    pub title: String,
    /// What the notes should say under this heading. May be empty: the title
    /// alone is often instruction enough.
    pub instructions: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCustomTemplate {
    pub template_id: MeetingTemplateId,
    pub name: String,
    /// What the template is for, in the person's own words. Read by the model
    /// as framing, never shown in the notes.
    pub purpose: String,
    pub sections: Vec<MeetingTemplateSection>,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
}

/// Every custom template, sorted by name ignoring case, and the revision every
/// write to the list is fenced on.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCustomTemplates {
    pub templates: Vec<MeetingCustomTemplate>,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCustomTemplateDraft {
    pub name: String,
    pub purpose: String,
    pub sections: Vec<MeetingTemplateSection>,
}

/// Create a template (`template_id: None`) or replace one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCustomTemplateSaveRequest {
    pub template_id: Option<MeetingTemplateId>,
    pub draft: MeetingCustomTemplateDraft,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCustomTemplateSaveResult {
    pub template_id: MeetingTemplateId,
    pub templates: MeetingCustomTemplates,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCustomTemplateDeleteRequest {
    pub template_id: MeetingTemplateId,
    pub expected_revision: u64,
}

/// The language generated notes are written in. `Auto` follows the meeting:
/// notes come out in the language most of it was spoken in.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingNotesLanguage {
    #[default]
    Auto,
    English,
}

impl MeetingNotesLanguage {
    /// The sentence the notes prompt ends with.
    pub const fn instruction(self) -> &'static str {
        match self {
            Self::Auto => {
                "Write every text you generate in the language most of the meeting was spoken in, even when these instructions are in another language; keep names, product names and quoted terms as they were said."
            }
            Self::English => {
                "Write every text you generate in English, whatever language the meeting was spoken in; keep names, product names and quoted terms as they were said."
            }
        }
    }
}

impl MeetingCustomTemplateDraft {
    /// The draft with every field trimmed, or `Err` when one breaks a limit.
    ///
    /// Section titles must differ ignoring case, because the notes validator
    /// matches the model's headings against them that way: two titles that
    /// read alike would leave it unable to say which section a heading means.
    pub fn normalized(&self) -> Result<Self, ()> {
        let name = one_line(&self.name, MAX_TEMPLATE_NAME_BYTES)?;
        let purpose = bounded(&self.purpose, MAX_TEMPLATE_PURPOSE_BYTES)?;
        if self.sections.is_empty() || self.sections.len() > MAX_TEMPLATE_SECTIONS {
            return Err(());
        }
        let mut sections: Vec<MeetingTemplateSection> = Vec::with_capacity(self.sections.len());
        for section in &self.sections {
            let title = one_line(&section.title, MAX_SECTION_TITLE_BYTES)?;
            if sections
                .iter()
                .any(|existing| same_section_title(&existing.title, &title))
            {
                return Err(());
            }
            sections.push(MeetingTemplateSection {
                title,
                instructions: bounded(&section.instructions, MAX_SECTION_INSTRUCTIONS_BYTES)?,
            });
        }
        Ok(Self {
            name,
            purpose,
            sections,
        })
    }
}

/// Whether two headings name the same section: trimmed, ignoring case.
pub fn same_section_title(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

fn one_line(value: &str, max_bytes: usize) -> Result<String, ()> {
    let value = value.trim();
    if value.is_empty() || value.len() > max_bytes || value.contains(['\n', '\r']) {
        return Err(());
    }
    Ok(value.to_string())
}

fn bounded(value: &str, max_bytes: usize) -> Result<String, ()> {
    let value = value.trim();
    if value.len() > max_bytes {
        return Err(());
    }
    Ok(value.to_string())
}

/// What one rung of the resolution chose: a built-in template, and the custom
/// template that overrides it when one is named and still exists.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NotesTemplateChoice {
    pub template: MeetingNotesTemplate,
    pub custom_template_id: Option<MeetingTemplateId>,
}

impl From<MeetingNotesTemplate> for NotesTemplateChoice {
    fn from(template: MeetingNotesTemplate) -> Self {
        Self {
            template,
            custom_template_id: None,
        }
    }
}

/// The template one generation is written to, with a custom template's
/// sections already read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotesTemplate {
    BuiltIn(MeetingNotesTemplate),
    Custom(MeetingCustomTemplate),
}

impl From<MeetingNotesTemplate> for NotesTemplate {
    fn from(template: MeetingNotesTemplate) -> Self {
        Self::BuiltIn(template)
    }
}

impl NotesTemplate {
    /// The id an artifact revision records and its generation key hashes.
    pub fn artifact_template_id(&self) -> String {
        match self {
            Self::BuiltIn(template) => template.artifact_template_id().to_string(),
            Self::Custom(template) => custom_artifact_template_id(template.template_id),
        }
    }
}

pub fn custom_artifact_template_id(template_id: MeetingTemplateId) -> String {
    format!("{CUSTOM_ARTIFACT_TEMPLATE_PREFIX}{}", template_id.uuid())
}

/// The custom template an artifact revision was written with, when it was.
pub fn custom_template_id_from_artifact(template_id: &str) -> Option<MeetingTemplateId> {
    template_id
        .strip_prefix(CUSTOM_ARTIFACT_TEMPLATE_PREFIX)
        .and_then(|uuid| Uuid::parse_str(uuid).ok())
        .map(MeetingTemplateId::from_uuid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(title: &str) -> MeetingTemplateSection {
        MeetingTemplateSection {
            title: title.to_string(),
            instructions: String::new(),
        }
    }

    fn draft(name: &str, sections: Vec<MeetingTemplateSection>) -> MeetingCustomTemplateDraft {
        MeetingCustomTemplateDraft {
            name: name.to_string(),
            purpose: String::new(),
            sections,
        }
    }

    /// The limits the editor mirrors, at their edges: the core is the
    /// authority, so a draft the editor let through and the core refuses is a
    /// save that fails with the field still on screen.
    #[test]
    fn a_draft_is_trimmed_and_held_to_its_limits() {
        let at_limits = MeetingCustomTemplateDraft {
            name: format!("  {}  ", "n".repeat(MAX_TEMPLATE_NAME_BYTES)),
            purpose: format!(" {} ", "p".repeat(MAX_TEMPLATE_PURPOSE_BYTES)),
            sections: (0..MAX_TEMPLATE_SECTIONS)
                .map(|index| MeetingTemplateSection {
                    title: format!(" Section {index} "),
                    instructions: "i".repeat(MAX_SECTION_INSTRUCTIONS_BYTES),
                })
                .collect(),
        };
        let normalized = at_limits
            .normalized()
            .expect("every field is at its ceiling");
        assert_eq!(normalized.name, "n".repeat(MAX_TEMPLATE_NAME_BYTES));
        assert_eq!(normalized.sections[3].title, "Section 3");

        let mut long_name = at_limits.clone();
        long_name.name = "n".repeat(MAX_TEMPLATE_NAME_BYTES + 1);
        assert!(long_name.normalized().is_err(), "a name one byte over");

        let mut long_instructions = at_limits.clone();
        long_instructions.sections[0].instructions = "i".repeat(MAX_SECTION_INSTRUCTIONS_BYTES + 1);
        assert!(
            long_instructions.normalized().is_err(),
            "instructions one byte over"
        );

        let mut too_many = at_limits.clone();
        too_many.sections.push(section("One more"));
        assert!(too_many.normalized().is_err(), "a ninth section");

        assert!(
            draft("Weekly", Vec::new()).normalized().is_err(),
            "no sections"
        );
        assert!(
            draft("   ", vec![section("Wins")]).normalized().is_err(),
            "a blank name"
        );
        assert!(draft("Two\nlines", vec![section("Wins")])
            .normalized()
            .is_err());
        assert!(
            draft("Weekly", vec![section(" ")]).normalized().is_err(),
            "a blank title"
        );
        assert!(
            draft("Weekly", vec![section("Wins"), section(" wINS ")])
                .normalized()
                .is_err(),
            "titles that differ only by case and spacing name one section"
        );
    }

    #[test]
    fn a_custom_artifact_id_names_its_template_and_nothing_else() {
        let template_id = MeetingTemplateId::new();
        let artifact_id = custom_artifact_template_id(template_id);
        assert_eq!(
            custom_template_id_from_artifact(&artifact_id),
            Some(template_id)
        );
        for built_in in MeetingNotesTemplate::ALL {
            assert_eq!(
                custom_template_id_from_artifact(built_in.artifact_template_id()),
                None
            );
        }
        assert_eq!(custom_template_id_from_artifact("custom:not-a-uuid"), None);
    }
}

//! Saved prompts: a question the operator wrote once, asked again on demand.
//!
//! A prompt is a name, a body, and what shape its answer takes. Nothing about
//! it is privileged: the three that ship are ordinary rows seeded by the
//! migration, editable and deletable like any other, so there is no second
//! class of built-in prompt whose behaviour a reader cannot inspect.
//!
//! `target` says which noun a prompt is written about — a meeting, a person, a
//! series — and is what ⌘K filters on when it offers prompts for whatever is
//! open. It is an offer, not a fence: an automation runs its prompt against the
//! meeting that just finished whatever the prompt says it is about, because
//! that is the only noun an after-meeting pass has.
//!
//! A run is kept forever beside its prompt and dies with it. That is the whole
//! lifecycle: a result is derived from the prompt, a prompt nobody kept has no
//! results worth reading, and the alternative — orphan rows with no name and no
//! re-run — is a surface that can only apologise.

use super::people_types::PersonId;
use super::types::{
    MeetingArtifactId, MeetingOperationId, MeetingSessionId, OperationReceipt, PromptRunId,
    SavedPromptId,
};
use serde::{Deserialize, Serialize};
use specta::Type;

/// The longest prompt name and body this app will store. A prompt is a
/// paragraph of instruction; past these it is a document that got in by
/// mistake, and refusing it at the boundary keeps a paste out of every model
/// input the prompt is ever used for.
const MAX_NAME_BYTES: usize = 120;
const MAX_BODY_BYTES: usize = 4 * 1024;
/// The longest JSON schema. A schema this app can check is a flat object with
/// a handful of properties; the ceiling is generous for that and still small
/// enough that a schema cannot become the model input.
const MAX_SCHEMA_BYTES: usize = 8 * 1024;

/// What shape a prompt's answer takes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptOutput {
    /// Prose, rendered as Markdown wherever a run is read.
    Text,
    /// One JSON object, checked against `json_schema` before it is stored. An
    /// answer that does not check is a failed run, never a stored half-answer.
    Schema { json_schema: String },
}

/// Which noun a prompt is written about.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PromptTarget {
    Meeting,
    Person,
    Series,
}

impl PromptTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Meeting => "meeting",
            Self::Person => "person",
            Self::Series => "series",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "meeting" => Some(Self::Meeting),
            "person" => Some(Self::Person),
            "series" => Some(Self::Series),
            _ => None,
        }
    }
}

/// One prompt the operator wrote.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SavedPrompt {
    pub prompt_id: SavedPromptId,
    pub name: String,
    pub body: String,
    pub output: PromptOutput,
    pub target: PromptTarget,
    /// Whether Talk to Sona and automations may call it by its name, as
    /// instructions to follow rather than a question about one meeting.
    #[serde(default)]
    pub skill: bool,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
}

impl SavedPromptList {
    /// The prompts marked as skills.
    pub fn skills(&self) -> impl Iterator<Item = &SavedPrompt> {
        self.prompts.iter().filter(|prompt| prompt.skill)
    }

    /// The skill called `name`, matched as a person would type it: case and
    /// surrounding spaces ignored. Skill names are unique, so there is at
    /// most one.
    pub fn skill(&self, name: &str) -> Option<&SavedPrompt> {
        let name = name.trim();
        self.skills()
            .find(|prompt| prompt.name.trim().eq_ignore_ascii_case(name))
    }
}

/// The prompts on this machine, and the fence a write against them carries.
///
/// One counter for the whole table, like the automations roster's: the settings
/// page and the palette both hold the whole list and write single rows against
/// it, and a per-row revision would cost a read each without changing what
/// happens when two windows save from the same list.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SavedPromptList {
    pub prompts: Vec<SavedPrompt>,
    pub revision: u64,
}

/// The noun one run was about.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptTargetRef {
    Meeting { session_id: MeetingSessionId },
    Person { person_id: PersonId },
    Series { series_key: String },
}

impl PromptTargetRef {
    pub const fn target(&self) -> PromptTarget {
        match self {
            Self::Meeting { .. } => PromptTarget::Meeting,
            Self::Person { .. } => PromptTarget::Person,
            Self::Series { .. } => PromptTarget::Series,
        }
    }

    /// How this noun is written in `saved_prompt_runs.target_id`: a uuid for
    /// the two that have one, and EventKit's calendar-item identifier for a
    /// series, which is the same string standing consent and D21 already use.
    pub fn id(&self) -> String {
        match self {
            Self::Meeting { session_id } => session_id.uuid().to_string(),
            Self::Person { person_id } => person_id.uuid().to_string(),
            Self::Series { series_key } => series_key.clone(),
        }
    }
}

/// Why a run produced no answer.
///
/// Its own vocabulary rather than [`super::types::MeetingReasonCode`], for the
/// same reason [`super::automation_types::MeetingAutomationFailure`] has one:
/// these are the ways one model call fails, and widening the app-wide code list
/// with them would make every unrelated receipt reader carry them too.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PromptRunFailure {
    /// No engine at all: remote is off or this series is excluded, and this
    /// machine has no on-device model either.
    ModelUnavailable,
    /// The chosen engine could not be reached.
    ModelUnreachable,
    /// It answered, and the answer was not usable.
    ModelFailed,
    /// A schema prompt whose answer was not JSON, or not this schema's JSON.
    SchemaMismatch,
    /// Nothing in the corpus for this prompt to read.
    NoEvidence,
}

impl PromptRunFailure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelUnavailable => "model_unavailable",
            Self::ModelUnreachable => "model_unreachable",
            Self::ModelFailed => "model_failed",
            Self::SchemaMismatch => "schema_mismatch",
            Self::NoEvidence => "no_evidence",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "model_unavailable" => Some(Self::ModelUnavailable),
            "model_unreachable" => Some(Self::ModelUnreachable),
            "model_failed" => Some(Self::ModelFailed),
            "schema_mismatch" => Some(Self::SchemaMismatch),
            "no_evidence" => Some(Self::NoEvidence),
            _ => None,
        }
    }
}

/// What one run produced.
///
/// Struct variants rather than the newtypes the shape suggests, because serde's
/// internal tagging — the convention every other tagged enum in this app uses —
/// cannot carry a newtype variant holding a string.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptRunResult {
    Text { text: String },
    Json { json: String },
    Failed { reason: PromptRunFailure },
}

impl PromptRunResult {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Json { .. } => "json",
            Self::Failed { .. } => "failed",
        }
    }
}

/// One attempt at one prompt, and its receipt.
///
/// This *is* the receipt, not a summary of one kept elsewhere — the same rule
/// [`super::automation_types::MeetingAutomationRunReceipt`] follows.
/// [`OperationReceipt`] is the currency of fenced preference writes; a run is a
/// generation, so it records its own outcome in its own row. Nothing retries: a
/// `Failed` row is the answer, and it stays visible.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PromptRun {
    pub run_id: PromptRunId,
    pub prompt_id: SavedPromptId,
    pub target_kind: PromptTarget,
    pub target_id: String,
    /// The notes revision this run read, when it read one meeting's own words.
    /// `None` for a prompt about a person or a series, which read a pack drawn
    /// from many.
    pub artifact_id: Option<MeetingArtifactId>,
    pub model_id: String,
    pub model_version: String,
    pub produced_at_utc_ms: i64,
    pub result: PromptRunResult,
}

/// Create a prompt, or rewrite one.
///
/// A `None` id asks for a new prompt; any other id is the row to overwrite, so
/// a replayed save from a stale window updates in place rather than duplicating.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SavedPromptSaveRequest {
    pub operation_id: MeetingOperationId,
    pub prompt_id: Option<SavedPromptId>,
    pub name: String,
    pub body: String,
    pub output: PromptOutput,
    pub target: PromptTarget,
    /// A skill's name may not be another skill's: a skill is called by it.
    #[serde(default)]
    pub skill: bool,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SavedPromptDeleteRequest {
    pub operation_id: MeetingOperationId,
    pub prompt_id: SavedPromptId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SavedPromptMutationResult {
    pub receipt: OperationReceipt,
    pub prompts: SavedPromptList,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SavedPromptRunRequest {
    pub prompt_id: SavedPromptId,
    pub target: PromptTargetRef,
}

/// A prompt as it will be stored, or why it will not be.
///
/// The one place that decides what a prompt may be, called on the way into the
/// store. A name and a body are required because a prompt without either is
/// unrunnable; a schema is parsed here rather than at generation time so the
/// operator finds out while they are still looking at the field.
pub fn normalized_prompt(
    request: &SavedPromptSaveRequest,
) -> Result<(String, String, PromptOutput), PromptInvalid> {
    let name = request.name.trim();
    let body = request.body.trim();
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(PromptInvalid);
    }
    if body.is_empty() || body.len() > MAX_BODY_BYTES {
        return Err(PromptInvalid);
    }
    let output = match &request.output {
        PromptOutput::Text => PromptOutput::Text,
        PromptOutput::Schema { json_schema } => {
            let json_schema = json_schema.trim();
            if json_schema.is_empty() || json_schema.len() > MAX_SCHEMA_BYTES {
                return Err(PromptInvalid);
            }
            let parsed: serde_json::Value =
                serde_json::from_str(json_schema).map_err(|_| PromptInvalid)?;
            if !parsed.is_object() {
                return Err(PromptInvalid);
            }
            PromptOutput::Schema {
                json_schema: json_schema.to_string(),
            }
        }
    };
    Ok((name.to_string(), body.to_string(), output))
}

/// The prompt cannot be stored as written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptInvalid;

/// Whether one answer satisfies one schema.
///
/// ponytail: a JSON Schema subset, not JSON Schema. What is checked is
/// `type: "object"` at the root, every name in `required` being present, and
/// each described property being a string, number, integer, boolean, or array
/// of strings. `$ref`, nested object schemas, `enum`, `format`, `oneOf`,
/// numeric bounds and `additionalProperties` are stored and not enforced. The
/// upgrade path is the `jsonschema` crate, which is not a dependency of this
/// app today; add it when a prompt needs a shape this cannot describe.
pub fn answer_matches_schema(schema: &serde_json::Value, answer: &serde_json::Value) -> bool {
    let Some(answer) = answer.as_object() else {
        return false;
    };
    if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
        // A schema that does not describe an object describes something this
        // subset cannot check, so the only honest answer is that the object we
        // asked for satisfies nothing.
        return false;
    }
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        for name in required {
            let Some(name) = name.as_str() else {
                return false;
            };
            if !answer.contains_key(name) {
                return false;
            }
        }
    }
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return true;
    };
    properties.iter().all(|(name, property)| {
        answer
            .get(name)
            .is_none_or(|value| value_matches(property, value))
    })
}

fn value_matches(property: &serde_json::Value, value: &serde_json::Value) -> bool {
    match property.get("type").and_then(serde_json::Value::as_str) {
        Some("string") => value.is_string(),
        Some("number") => value.is_number(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("boolean") => value.is_boolean(),
        Some("array") => value.as_array().is_some_and(|items| {
            let described = property.get("items");
            items
                .iter()
                .all(|item| described.is_none_or(|items| value_matches(items, item)))
        }),
        // A property this subset does not describe is not a property this
        // subset may refuse.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn save(name: &str, body: &str, output: PromptOutput) -> SavedPromptSaveRequest {
        SavedPromptSaveRequest {
            operation_id: MeetingOperationId::new(),
            prompt_id: None,
            name: name.to_string(),
            body: body.to_string(),
            output,
            target: PromptTarget::Meeting,
            skill: false,
            expected_revision: 0,
        }
    }

    #[test]
    fn a_prompt_needs_both_a_name_and_a_body() {
        assert!(normalized_prompt(&save("  ", "What was decided?", PromptOutput::Text)).is_err());
        assert!(normalized_prompt(&save("Decisions", "   ", PromptOutput::Text)).is_err());
    }

    #[test]
    fn normalization_trims_and_keeps_the_schema_verbatim() {
        let schema = "{\"type\":\"object\"}";
        let (name, body, output) = normalized_prompt(&save(
            "  Decisions  ",
            "  List the decisions.  ",
            PromptOutput::Schema {
                json_schema: format!("  {schema}  "),
            },
        ))
        .expect("usable");

        assert_eq!(name, "Decisions");
        assert_eq!(body, "List the decisions.");
        assert_eq!(
            output,
            PromptOutput::Schema {
                json_schema: schema.to_string()
            }
        );
    }

    #[test]
    fn a_schema_that_is_not_json_is_refused_at_the_boundary() {
        assert!(normalized_prompt(&save(
            "Decisions",
            "List them.",
            PromptOutput::Schema {
                json_schema: "{not json".to_string()
            }
        ))
        .is_err());
        assert!(normalized_prompt(&save(
            "Decisions",
            "List them.",
            PromptOutput::Schema {
                json_schema: "[1, 2]".to_string()
            }
        ))
        .is_err());
    }

    #[test]
    fn required_keys_and_property_types_are_checked() {
        let schema = json!({
            "type": "object",
            "required": ["decisions"],
            "properties": {
                "decisions": { "type": "array", "items": { "type": "string" } },
                "confident": { "type": "boolean" }
            }
        });

        assert!(answer_matches_schema(
            &schema,
            &json!({ "decisions": ["ship on Friday"], "confident": true })
        ));
        // The optional property may be absent; the required one may not.
        assert!(answer_matches_schema(&schema, &json!({ "decisions": [] })));
        assert!(!answer_matches_schema(
            &schema,
            &json!({ "confident": true })
        ));
        // Right key, wrong shape.
        assert!(!answer_matches_schema(
            &schema,
            &json!({ "decisions": "ship on Friday" })
        ));
        assert!(!answer_matches_schema(
            &schema,
            &json!({ "decisions": [1, 2] })
        ));
        // Not an object at all.
        assert!(!answer_matches_schema(&schema, &json!(["ship"])));
    }
}

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::agent_hook_wire::CanonicalEvent;

#[derive(Debug, Clone, Deserialize, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentBridgeQuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentBridgeQuestion {
    pub header: String,
    pub question: String,
    pub options: Vec<AgentBridgeQuestionOption>,
    pub multi_select: bool,
    pub custom: bool,
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Type)]
#[serde(
    tag = "decision",
    content = "answers",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AgentBridgeQuestionResponse {
    Answer(Vec<Vec<String>>),
    Reject,
}

pub(super) fn from_event(event: &CanonicalEvent) -> Option<Vec<AgentBridgeQuestion>> {
    if !event.is_opencode_question() {
        return None;
    }
    let input = event.tool.as_ref()?.input.as_ref()?.get("questions")?;
    let questions = Vec::<AgentBridgeQuestion>::deserialize(input).ok()?;
    if questions.is_empty()
        || questions.iter().any(|question| {
            question.question.trim().is_empty()
                || (!question.custom && question.options.is_empty())
                || question.options.iter().enumerate().any(|(index, option)| {
                    option.label.trim().is_empty()
                        || question.options[..index]
                            .iter()
                            .any(|previous| previous.label == option.label)
                })
        })
    {
        return None;
    }
    Some(questions)
}

pub(super) fn answers_match(questions: &[AgentBridgeQuestion], answers: &[Vec<String>]) -> bool {
    questions.len() == answers.len()
        && questions.iter().zip(answers).all(|(question, answer)| {
            (!question.required || !answer.is_empty())
                && (question.multi_select || answer.len() <= 1)
                && answer.iter().enumerate().all(|(index, selected)| {
                    !selected.trim().is_empty()
                        && !answer[..index].contains(selected)
                        && (question.custom
                            || question
                                .options
                                .iter()
                                .any(|option| option.label == *selected))
                })
        })
}

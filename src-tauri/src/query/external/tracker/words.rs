//! `sona work list --where`: filter words read as the conditions the list
//! filters by, with the words that were not understood.

use super::{refusal, Tracker};
use crate::planning::work::filter::{WorkCondition, WorkFilterField};
use crate::planning::work::words::work_where;
use crate::query::external::ExternalError;
use serde::Serialize;
use std::fmt::Write as _;
use tauri::Manager;

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkWhere {
    /// What the words were read as. Every task listed matches all of them.
    pub terms: Vec<ExternalWorkWhereTerm>,
    /// Words read as nothing. They did not narrow the list.
    pub not_understood: Vec<ExternalWorkWhereWord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkWhereTerm {
    pub text: String,
    pub reads_as: String,
    /// The same condition `--filter` takes.
    pub condition: WorkCondition,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkWhereWord {
    pub text: String,
    /// What a word naming more than one thing could mean.
    pub could_be: Vec<String>,
}

impl ExternalWorkWhere {
    pub(super) fn conditions(&self) -> impl Iterator<Item = WorkCondition> + '_ {
        self.terms.iter().map(|term| term.condition.clone())
    }

    pub(super) fn names(&self, field: WorkFilterField) -> bool {
        self.terms.iter().any(|term| term.condition.field == field)
    }
}

impl Tracker<'_> {
    pub(super) async fn where_words(&self, text: &str) -> Result<ExternalWorkWhere, ExternalError> {
        let read = work_where(self.app.state(), self.app.state(), text.to_owned())
            .await
            .map_err(|message| refusal(self.history, message))?;
        Ok(ExternalWorkWhere {
            terms: read
                .terms
                .into_iter()
                .map(|term| ExternalWorkWhereTerm {
                    text: term.text,
                    reads_as: term.label,
                    condition: term.condition,
                })
                .collect(),
            not_understood: read
                .unread
                .into_iter()
                .map(|word| ExternalWorkWhereWord {
                    text: word.text,
                    could_be: word.could_be,
                })
                .collect(),
        })
    }
}

pub(super) fn text(read_as: &ExternalWorkWhere, text: &mut String) {
    if !read_as.terms.is_empty() {
        let terms: Vec<&str> = read_as
            .terms
            .iter()
            .map(|term| term.reads_as.as_str())
            .collect();
        let _ = writeln!(text, "Read as: {}", terms.join(" · "));
    }
    for word in &read_as.not_understood {
        if word.could_be.is_empty() {
            let _ = writeln!(text, "Not understood: \"{}\"", word.text);
        } else {
            let _ = writeln!(
                text,
                "Not understood: \"{}\" could be {}",
                word.text,
                word.could_be.join(" or ")
            );
        }
    }
}

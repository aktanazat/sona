//! The chat's own memory: the last twenty conversations, on disk beside the
//! settings store.
//!
//! The file is the only copy. There is no in-memory mirror to go stale, and a
//! read-modify-write per finished turn costs one small `read` against a
//! network round trip that already happened. What the panel holds in
//! [`super::PanelState`] is the conversation currently on screen, which is a
//! different fact from the twenty the reader can go back to.
//!
//! Nothing here writes a receipt. A receipt records a change the app made on
//! the reader's behalf to something the reader owns; this file is the panel's
//! own scrollback, written by the act of asking, and a ledger row per keystroke
//! would drown the ledger the meeting store keeps.

use super::protocol::{SonaAgentChatRoleV1, SonaAgentChatTurnV1};
use crate::fs_util::write_private_file;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};

/// How many conversations survive. Past this the oldest is forgotten: the
/// history button is a way back to this morning, not an archive, and an
/// unbounded file would grow with every question ever asked.
pub(crate) const MAX_STORED_CONVERSATIONS: usize = 20;

/// The widest a title may be, counted in characters rather than bytes so a
/// question in Japanese is cut where a reader would cut it. The last character
/// of a cut title is the ellipsis, so the budget is never exceeded.
const MAX_TITLE_CHARS: usize = 48;

const HISTORY_FILE_NAME: &str = "agent_chat_history.json";
const HISTORY_SCHEMA_VERSION: u32 = 1;

/// One remembered conversation, whole. A file record rather than a wire type:
/// the sheet is handed a conversation's turns, never the row that holds them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentChatConversationV1 {
    pub conversation_id: String,
    pub title: String,
    pub turns: Vec<SonaAgentChatTurnV1>,
    pub updated_at_utc_ms: i64,
}

/// One row of the history popover: enough to choose by, and no transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentChatConversationSummaryV1 {
    pub conversation_id: String,
    pub title: String,
    pub updated_at_utc_ms: i64,
}

/// The file, as it is written.
///
/// `deny_unknown_fields` and the version check are deliberate: a file this
/// build cannot read in full is treated as no file at all, and the next write
/// replaces it. History is disposable — refusing to guess at a half-understood
/// record is cheaper than showing a reader a conversation with pieces missing.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatHistoryFileV1 {
    schema_version: u32,
    conversations: Vec<AgentChatConversationV1>,
}

/// The first thing the reader said, as the name of the exchange it started.
///
/// Not the assistant's answer: the answer is what you go looking for, the
/// question is what you remember asking. Newlines fold into spaces because a
/// popover row is one line and a pasted paragraph would otherwise arrive with
/// its shape intact and its meaning lost.
pub(crate) fn conversation_title(turns: &[SonaAgentChatTurnV1]) -> String {
    let Some(first) = turns
        .iter()
        .find(|turn| turn.role == SonaAgentChatRoleV1::User)
    else {
        return String::new();
    };
    let flattened = first
        .message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flattened.chars().count() <= MAX_TITLE_CHARS {
        return flattened;
    }
    let mut title: String = flattened.chars().take(MAX_TITLE_CHARS - 1).collect();
    title.push('…');
    title
}

/// Put one conversation at the front, replacing any earlier copy of itself,
/// and drop whatever falls off the end.
///
/// Newest first is the order the popover reads in, so it is the order the file
/// is written in: a list that has to be sorted on the way out is a list that
/// can be sorted two different ways.
pub(crate) fn upsert(
    conversations: &mut Vec<AgentChatConversationV1>,
    entry: AgentChatConversationV1,
) {
    conversations.retain(|held| held.conversation_id != entry.conversation_id);
    conversations.insert(0, entry);
    conversations.truncate(MAX_STORED_CONVERSATIONS);
}

/// What the bytes on disk say, or nothing at all.
pub(crate) fn decode(bytes: &[u8]) -> Vec<AgentChatConversationV1> {
    let Ok(file) = serde_json::from_slice::<ChatHistoryFileV1>(bytes) else {
        return Vec::new();
    };
    if file.schema_version != HISTORY_SCHEMA_VERSION {
        return Vec::new();
    }
    let mut conversations = file.conversations;
    conversations.truncate(MAX_STORED_CONVERSATIONS);
    conversations
}

pub(crate) fn encode(conversations: &[AgentChatConversationV1]) -> Vec<u8> {
    serde_json::to_vec(&ChatHistoryFileV1 {
        schema_version: HISTORY_SCHEMA_VERSION,
        conversations: conversations.to_vec(),
    })
    .unwrap_or_default()
}

pub(crate) fn read_at(path: &Path) -> Vec<AgentChatConversationV1> {
    std::fs::read(path).map_or_else(|_| Vec::new(), |bytes| decode(&bytes))
}

/// Replace the file, or leave the one that is there untouched.
///
/// The same rename-over-a-private-temporary every other durable record in this
/// app is written with, so a crash mid-write loses the newest turn rather than
/// the last twenty conversations.
pub(crate) fn write_at(
    path: &Path,
    conversations: &[AgentChatConversationV1],
) -> std::io::Result<()> {
    write_private_file(path, &encode(conversations))
}

fn history_path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    if let Some(dir) = crate::portable::data_dir() {
        Some(dir.join(HISTORY_FILE_NAME))
    } else {
        app.path()
            .app_data_dir()
            .ok()
            .map(|dir| dir.join(HISTORY_FILE_NAME))
    }
}

pub(crate) fn list<R: Runtime>(app: &AppHandle<R>) -> Vec<AgentChatConversationSummaryV1> {
    let Some(path) = history_path(app) else {
        return Vec::new();
    };
    read_at(&path)
        .into_iter()
        .map(|conversation| AgentChatConversationSummaryV1 {
            conversation_id: conversation.conversation_id,
            title: conversation.title,
            updated_at_utc_ms: conversation.updated_at_utc_ms,
        })
        .collect()
}

pub(crate) fn turns_of<R: Runtime>(
    app: &AppHandle<R>,
    conversation_id: &str,
) -> Option<Vec<SonaAgentChatTurnV1>> {
    let path = history_path(app)?;
    read_at(&path)
        .into_iter()
        .find(|conversation| conversation.conversation_id == conversation_id)
        .map(|conversation| conversation.turns)
}

/// Record where this conversation has got to.
///
/// An empty exchange is not a conversation: `new chat` followed by nothing must
/// not leave an untitled row in the popover, so nothing is written until the
/// reader has actually said something.
pub(crate) fn remember<R: Runtime>(
    app: &AppHandle<R>,
    conversation_id: &str,
    turns: &[SonaAgentChatTurnV1],
) {
    if turns.is_empty() {
        return;
    }
    let Some(path) = history_path(app) else {
        return;
    };
    let mut conversations = read_at(&path);
    upsert(
        &mut conversations,
        AgentChatConversationV1 {
            conversation_id: conversation_id.to_string(),
            title: conversation_title(turns),
            turns: turns.to_vec(),
            updated_at_utc_ms: chrono::Utc::now().timestamp_millis(),
        },
    );
    if let Err(error) = write_at(&path, &conversations) {
        log::warn!("Failed to persist agent chat history: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(message: &str) -> SonaAgentChatTurnV1 {
        SonaAgentChatTurnV1 {
            role: SonaAgentChatRoleV1::User,
            message: message.to_string(),
            outcome: None,
        }
    }

    fn assistant(message: &str) -> SonaAgentChatTurnV1 {
        SonaAgentChatTurnV1 {
            role: SonaAgentChatRoleV1::Assistant,
            message: message.to_string(),
            outcome: None,
        }
    }

    fn conversation(id: &str, turns: Vec<SonaAgentChatTurnV1>) -> AgentChatConversationV1 {
        AgentChatConversationV1 {
            conversation_id: id.to_string(),
            title: conversation_title(&turns),
            turns,
            updated_at_utc_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn the_title_is_the_question_not_the_answer() {
        assert_eq!(
            conversation_title(&[
                assistant("Here is what I found."),
                user("What did we decide about pricing?"),
            ]),
            "What did we decide about pricing?"
        );
        assert_eq!(conversation_title(&[assistant("orphan")]), "");
    }

    #[test]
    fn a_long_question_is_cut_to_the_title_budget() {
        let title = conversation_title(&[user(&"a".repeat(200))]);

        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert!(title.ends_with('…'));
    }

    /// A pasted paragraph arrives with newlines and runs of spaces in it. A
    /// popover row is one line, so the title is one line before it is stored
    /// rather than being squashed by whichever surface happens to draw it.
    #[test]
    fn a_pasted_question_is_flattened_to_one_line() {
        assert_eq!(
            conversation_title(&[user("  What did\n\nwe   decide? ")]),
            "What did we decide?"
        );
    }

    #[test]
    fn the_newest_conversation_is_first_and_the_twenty_first_falls_off() {
        let mut conversations = Vec::new();
        for index in 0..(MAX_STORED_CONVERSATIONS + 5) {
            upsert(
                &mut conversations,
                conversation(&format!("c{index}"), vec![user(&format!("q{index}"))]),
            );
        }

        assert_eq!(conversations.len(), MAX_STORED_CONVERSATIONS);
        assert_eq!(conversations[0].conversation_id, "c24");
        assert_eq!(
            conversations[MAX_STORED_CONVERSATIONS - 1].conversation_id,
            "c5"
        );
    }

    /// The same conversation asked a second question is one row that moved, not
    /// two rows that disagree.
    #[test]
    fn a_conversation_that_grows_replaces_itself_and_moves_to_the_front() {
        let mut conversations = vec![
            conversation("older", vec![user("first")]),
            conversation("current", vec![user("second")]),
        ];
        upsert(
            &mut conversations,
            conversation("older", vec![user("first"), assistant("answer")]),
        );

        assert_eq!(conversations.len(), 2);
        assert_eq!(conversations[0].conversation_id, "older");
        assert_eq!(conversations[0].turns.len(), 2);
    }

    #[test]
    fn a_written_file_reads_back_as_what_was_written() {
        let directory = tempfile::tempdir().expect("temporary history root");
        let path = directory.path().join("nested").join(HISTORY_FILE_NAME);
        let conversations = vec![conversation("c1", vec![user("hello"), assistant("hi")])];

        write_at(&path, &conversations).expect("write history");

        assert_eq!(read_at(&path), conversations);
    }

    /// The write must not be able to leave a half-file behind, and it must not
    /// leave its temporary lying beside the record either.
    #[test]
    fn the_write_replaces_the_file_and_keeps_no_temporary() {
        let directory = tempfile::tempdir().expect("temporary history root");
        let path = directory.path().join(HISTORY_FILE_NAME);
        write_at(&path, &[conversation("c1", vec![user("one")])]).expect("first write");
        write_at(&path, &[conversation("c2", vec![user("two")])]).expect("second write");

        let entries = std::fs::read_dir(directory.path())
            .expect("read history root")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(entries, vec![HISTORY_FILE_NAME.to_string()]);
        assert_eq!(read_at(&path)[0].conversation_id, "c2");
    }

    #[test]
    fn legacy_turns_without_an_outcome_still_load() {
        let legacy = br#"{"schema_version":1,"conversations":[{"conversation_id":"legacy","title":"Question","turns":[{"role":"user","message":"Question"}],"updated_at_utc_ms":1700000000000}]}"#;

        let conversations = decode(legacy);

        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].turns, vec![user("Question")]);
    }

    #[test]
    fn a_rate_limited_failure_survives_the_disk_round_trip() {
        use crate::agent_panel::protocol::{AgentPanelTurnFailureV1, SonaAgentChatOutcomeV1};

        let directory = tempfile::tempdir().expect("temporary history root");
        let path = directory.path().join(HISTORY_FILE_NAME);
        let mut question = user("Try the relay");
        question.outcome = Some(SonaAgentChatOutcomeV1::Failure {
            failure: AgentPanelTurnFailureV1::RateLimited,
        });
        let conversations = vec![conversation("rate-limited", vec![question])];

        write_at(&path, &conversations).expect("write failed turn");

        assert_eq!(read_at(&path), conversations);
        assert!(std::fs::read_to_string(&path)
            .expect("read history JSON")
            .contains("rate_limited"));
    }

    #[test]
    fn an_unreadable_or_foreign_file_reads_as_no_history() {
        let directory = tempfile::tempdir().expect("temporary history root");
        let missing = directory.path().join("absent.json");
        let corrupt = directory.path().join("corrupt.json");
        let future = directory.path().join("future.json");
        std::fs::write(&corrupt, b"{not json").expect("corrupt fixture");
        std::fs::write(&future, br#"{"schema_version":2,"conversations":[]}"#)
            .expect("future fixture");

        assert!(read_at(&missing).is_empty());
        assert!(read_at(&corrupt).is_empty());
        assert!(read_at(&future).is_empty());
    }
}

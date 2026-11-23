use super::super::rows;
use super::*;
use crate::meeting::local_generator::LocalEndpointGenerator;
use crate::meeting::processing::MeetingTextGenerationError;
use crate::planning::schema;
use std::borrow::Cow;
use std::sync::Mutex;
use std::time::Instant;

/// Synthetic comments an agent might leave, and whether each waits on the
/// user. Written before the prompt was tuned and not quoted in it. Half
/// ask; the second half of each group is the hard kind: asks with no
/// question mark and no stock phrase, and tells that carry one.
pub(crate) const LABELLED: [(&str, bool); 64] = [
    // Plain asks.
    ("Do you want the invoice sent to finance or to the vendor directly?", true),
    ("Which of the two logos should go on the cover slide?", true),
    ("Should I keep the old pricing table or remove it?", true),
    ("Can you share the staging password so I can run the import?", true),
    ("Is it fine to delete the 2023 archive folder?", true),
    ("Please confirm the headcount for the offsite before I reserve rooms.", true),
    ("I need your approval to merge the billing change.", true),
    ("Waiting on you for the final copy of the announcement.", true),
    ("Let me know which date works and I will send the invites.", true),
    ("Three caterers quoted. Your call on which one.", true),
    ("What budget should I assume for the booth?", true),
    ("Who should be the reviewer for the security section?", true),
    ("Are we targeting iOS first, or both platforms at once?", true),
    ("Please decide whether the beta stays invite-only.", true),
    ("Does the contract need legal review before signing?", true),
    ("Blocked on you: I do not have access to the analytics workspace.", true),
    // Asks with no question mark and no stock phrase.
    ("Holding the release until I hear which changelog wording you prefer.", true),
    ("I can't proceed without the signed NDA from your side.", true),
    ("Not sure if the refund should be full or partial; paused here.", true),
    ("Drafted two subject lines, A and B. Pick one and I'll schedule the send.", true),
    ("This requires a decision on whether to keep supporting the legacy plan.", true),
    ("The deploy is ready but I won't ship it until you give the go-ahead.", true),
    ("Missing the API key for the mail provider. Stopped at step 3.", true),
    ("I found two possible owners for this task and did not want to guess.", true),
    ("Needs sign-off from you before the vendor is paid.", true),
    ("Stuck: the design file is view-only for me, so the export step is pending your access grant.", true),
    ("Tell me the tone you want and I will rewrite the intro.", true),
    ("Paused the migration at the halfway point pending your OK on the downtime window.", true),
    ("I would rather not choose the launch city myself; options are Austin and Denver.", true),
    ("Awaiting your answer on the seating chart to finish this.", true),
    ("Cannot tell from the notes whether the client approved the scope. Parking this until that is clear from you.", true),
    ("Either the Tuesday or the Thursday slot works for the room; reserve neither until you choose.", true),
    // Plain tells.
    ("Updated the roadmap doc with the Q3 dates.", false),
    ("Merged the copy fixes and closed the related tasks.", false),
    ("Attached the receipt and marked the expense as filed.", false),
    ("Renamed the task to match the project naming.", false),
    ("Sent the reminder to the venue; they confirmed by email.", false),
    ("Imported 42 contacts from the spreadsheet.", false),
    ("Set the due date to Friday as discussed in the sync.", false),
    ("Linked this to the onboarding checklist.", false),
    ("Summarized the call notes in the description.", false),
    ("Moved this to Done after the build passed.", false),
    ("Added labels for marketing and launch.", false),
    ("Booked the 10am slot and added it to the calendar.", false),
    ("Cleaned up duplicate entries in the vendor list.", false),
    ("Reordered the subtasks by dependency.", false),
    ("Exported the report as a PDF and attached it.", false),
    ("Archived the three stale drafts.", false),
    // Tells that look like asks.
    ("Which region was failing? It was eu-west; restarted the worker and it recovered.", false),
    ("Was the invoice paid? Yes, the bank shows it cleared on the 3rd.", false),
    ("The customer asked 'can we get a discount?' and I logged it in the CRM notes.", false),
    ("Let me know if anything looks off, otherwise this is finished.", false),
    ("Waiting on the vendor's reply; nothing needed from you.", false),
    ("Found the answer in the docs: https://example.com/help?topic=export&page=2", false),
    ("Blocked on the CI queue for a few minutes, then it went through.", false),
    ("Your call with the design team is on the calendar for Monday.", false),
    ("No approval needed for this one, so I merged it.", false),
    ("Should this have been split? Probably, so I split it into two tasks.", false),
    ("I decided to keep the older file since it had the signatures.", false),
    ("Happy to redo the chart in another style if you ever want; done for now.", false),
    ("Waiting on Priya from finance for the numbers; I pinged her.", false),
    ("FAQ entry added: 'How do I reset my password?'", false),
    ("Confirmed with the venue that parking is free.", false),
    ("The open question about pricing was settled in yesterday's sync; recorded the outcome.", false),
];

/// A model that answers from a script, and remembers what it was asked.
struct Scripted {
    replies: Mutex<Vec<Result<String, MeetingTextGenerationError>>>,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(replies: &[Option<&str>]) -> Self {
        Self {
            replies: Mutex::new(
                replies
                    .iter()
                    .rev()
                    .map(|reply| {
                        reply
                            .map(str::to_owned)
                            .ok_or(MeetingTextGenerationError::Failed)
                    })
                    .collect(),
            ),
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl MeetingTextGenerator for Scripted {
    fn is_available(&self) -> bool {
        true
    }
    fn model_id(&self) -> &'static str {
        "scripted"
    }
    fn model_version(&self) -> Cow<'static, str> {
        Cow::Borrowed("scripted-1")
    }
    fn max_input_bytes(&self) -> usize {
        usize::MAX
    }
    fn generate(
        &self,
        _system_prompt: &str,
        evidence: &str,
        _max_tokens: i32,
        _shape: ReplyShape,
    ) -> Result<String, MeetingTextGenerationError> {
        // PANIC: a poisoned fixture lock is a broken test.
        self.asked.lock().expect("asked").push(evidence.to_owned());
        self.replies
            .lock()
            .expect("replies")
            .pop()
            .unwrap_or(Err(MeetingTextGenerationError::Failed))
    }
}

#[test]
fn a_verdict_is_read_from_the_reply_and_nothing_else_counts() {
    let model = Scripted::new(&[
        Some(r#"{"asks": true}"#),
        Some("Sure. {\"asks\": false} Hope that helps."),
        Some("yes"),
        Some(r#"{"asks": "maybe"}"#),
        None,
    ]);
    assert_eq!(judge(&model, "a"), Some(true));
    assert_eq!(judge(&model, "b"), Some(false));
    assert_eq!(judge(&model, "c"), None);
    assert_eq!(judge(&model, "d"), None);
    assert_eq!(judge(&model, "e"), None);
}

#[test]
fn a_long_comment_is_read_at_both_ends() {
    let model = Scripted::new(&[Some(r#"{"asks": true}"#)]);
    let body = format!(
        "Start é. {} Which one do you want?",
        "é filler. ".repeat(600)
    );
    assert_eq!(judge(&model, &body), Some(true));
    let asked = model.asked.lock().expect("asked");
    assert!(asked[0].len() <= READ_BYTES + 32);
    assert!(asked[0].starts_with("Comment: Start é."));
    assert!(asked[0].ends_with("Which one do you want?"));
}

fn history() -> Connection {
    // PANIC: a fixture that cannot open its database is a broken test.
    let history = Connection::open_in_memory().expect("an in-memory history database");
    schema::apply_all(&history).expect("the planner schema");
    history
}

fn comment(history: &Connection, id: &str, body: &str, at_ms: i64, read: bool) {
    history
        .execute(
            "INSERT INTO planning_task_comments (id, task_id, author_kind, author_name, body, created_at_ms)
             VALUES (?1, 'task', 'agent', 'Scout', ?2, ?3)",
            params![id, body, at_ms],
        )
        .expect("the comment");
    history
        .execute(
            "INSERT INTO planning_notifications (id, event_key, task_id, kind, actor, comment_id, created_at_ms, read_at_ms)
             VALUES (?1, ?1, 'task', 'comment', 'agent', ?2, ?3, ?4)",
            params![format!("notice-{id}"), id, at_ms, read.then_some(at_ms)],
        )
        .expect("the notice");
}

#[test]
fn a_verdict_is_kept_for_the_words_it_judged_and_dropped_with_them() {
    let history = history();
    comment(&history, "old", "Read already.", 1, true);
    comment(
        &history,
        "a",
        "I can't proceed without the signed NDA.",
        2,
        false,
    );
    comment(
        &history,
        "b",
        "Let me know if anything looks off.",
        3,
        false,
    );

    let waiting = unjudged(&history, 10).expect("the waiting comments");
    assert_eq!(
        waiting.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["b", "a"],
        "newest first, and nothing the user already read"
    );
    assert!(keep(&history, &waiting[0], false, "m", 9).expect("kept"));
    assert!(keep(&history, &waiting[1], true, "m", 9).expect("kept"));
    assert!(unjudged(&history, 10).expect("none waiting").is_empty());
    let verdicts = judged(&history).expect("the verdicts");
    assert_eq!(verdicts.get("a"), Some(&true));
    assert_eq!(verdicts.get("b"), Some(&false));

    // The model's verdict wins over the rule in both directions.
    let context = rows::context(&history).expect("the context");
    assert!(context.asks("a"), "no question mark, no stock phrase");
    assert!(!context.asks("b"), "a stock phrase the user may ignore");
    assert!(!rows::asks("I can't proceed without the signed NDA."));
    assert!(rows::asks("Let me know if anything looks off."));

    // An edit retires the verdict, and a late verdict for the old words is refused.
    history
        .execute(
            "UPDATE planning_task_comments SET body = 'Done.', edited_at_ms = 20 WHERE id = 'a'",
            [],
        )
        .expect("the edit");
    assert_eq!(judged(&history).expect("the verdicts").get("a"), None);
    assert!(!keep(&history, &waiting[1], true, "m", 21).expect("refused"));
    assert!(!rows::context(&history).expect("the context").asks("a"));
    assert_eq!(unjudged(&history, 10).expect("waiting again").len(), 1);

    history
        .execute("DELETE FROM planning_task_comments WHERE id = 'b'", [])
        .expect("the removal");
    let left: i64 = history
        .query_row("SELECT count(*) FROM planning_comment_asks", [], |row| {
            row.get(0)
        })
        .expect("the count");
    assert_eq!(left, 1, "only the stale verdict for the edited comment");
}

fn score(verdicts: &[Option<bool>]) -> (usize, usize, usize, usize) {
    let (mut right, mut flagged, mut flagged_right, mut unanswered) = (0, 0, 0, 0);
    for (verdict, (_, asks)) in verdicts.iter().zip(LABELLED) {
        match verdict {
            Some(said) => {
                right += usize::from(*said == asks);
                flagged += usize::from(*said);
                flagged_right += usize::from(*said && asks);
            }
            None => unanswered += 1,
        }
    }
    (right, flagged, flagged_right, unanswered)
}

#[test]
fn the_rule_scores_what_it_scores_on_the_labelled_comments() {
    let verdicts: Vec<_> = LABELLED
        .iter()
        .map(|(body, _)| Some(rows::asks(body)))
        .collect();
    let (right, flagged, flagged_right, _) = score(&verdicts);
    println!(
        "rule:  {right}/64 right, precision {flagged_right}/{flagged}, recall {flagged_right}/32"
    );
    // What the rule scores today, to catch a slide in the fallback.
    assert!(right >= 40, "rule accuracy {right}/64");
}

/// Run against a local model:
/// `SONA_ASKS_ENDPOINT=http://127.0.0.1:11434/v1 SONA_ASKS_MODEL=<model>
///  bun run test:backend -- asks::tests::a_local_model -- --ignored --nocapture`
#[test]
#[ignore = "needs a local model endpoint"]
fn a_local_model_scores_the_labelled_comments() {
    // PANIC: this test is run by hand with both variables set.
    let endpoint = std::env::var("SONA_ASKS_ENDPOINT").expect("SONA_ASKS_ENDPOINT");
    let model = std::env::var("SONA_ASKS_MODEL").expect("SONA_ASKS_MODEL");
    let generator = LocalEndpointGenerator::new_with_context(&endpoint, &model, Some(4096))
        .expect("the endpoint");
    assert!(
        generator.is_available(),
        "{model} is not served at {endpoint}"
    );
    judge(&generator, "Warm the model.");
    let mut took_ms = Vec::new();
    let verdicts: Vec<_> = LABELLED
        .iter()
        .map(|(body, asks)| {
            let began = Instant::now();
            let verdict = judge(&generator, body);
            took_ms.push(began.elapsed().as_millis());
            if verdict != Some(*asks) {
                println!("  wrong ({verdict:?}, labelled {asks}): {body}");
            }
            verdict
        })
        .collect();
    took_ms.sort_unstable();
    let (right, flagged, flagged_right, unanswered) = score(&verdicts);
    println!(
        "{model}: {right}/64 right, precision {flagged_right}/{flagged}, recall {flagged_right}/32, {unanswered} unanswered"
    );
    println!(
        "{model}: one comment median {} ms, p95 {} ms",
        took_ms[took_ms.len() / 2],
        took_ms[took_ms.len() * 95 / 100]
    );
}

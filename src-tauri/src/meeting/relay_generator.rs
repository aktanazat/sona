//! D14: the meeting-intelligence engine that runs on the operator's own
//! server.
//!
//! One more [`MeetingTextGenerator`], with the agent panel's relay as its
//! transport: the same Ed25519-signed requests, the same tailnet-or-loopback
//! allowlist, the same submit-and-poll job shape, the same pinned relay key.
//! Nothing about the wire lives here — [`crate::agent_panel::run_chat_turn`]
//! owns that, and this module owns only the shape of the question and what its
//! two failures mean to a meeting.
//!
//! Selection lives in `processing::choose_text_engine`, not here. This engine
//! reports whether it *could* run; whether it *should* is the operator's
//! setting, their pairing, and the series' own answer, and one place decides
//! all three.

use super::processing::{MeetingTextGenerationError, MeetingTextGenerator, ReplyShape};
use crate::agent_panel::{self, ChatTurnError};
use std::future::Future;
use std::sync::mpsc;
use std::time::Duration;
use tauri::AppHandle;

/// What an artifact records as the engine that wrote it. Stable, and hashed
/// into every generation key, so it may not change without retiring every
/// generation this engine has produced.
const RELAY_MODEL_ID: &str = "sona-relay";

/// The engine's own version, as far as an artifact is concerned.
///
/// A generation key has to be known *before* the call that produces the text,
/// so this cannot be read out of a response. The relay's job envelope carries
/// no brain version either — its only model identity is the pinned `ultra`
/// alias, which is a routing constant rather than a version. So `v1` is this
/// client's turn shape: it bumps when the question this engine asks changes,
/// which is exactly when a cached generation should be retired.
const RELAY_MODEL_VERSION: &str = "v1";

/// The largest serialized model input this engine may send, in bytes.
///
/// Two ceilings sit behind this number, both enforced independently by the
/// relay and both fail-closed: 128 KiB for the context pack on its own, and
/// 184 KiB for the whole canonical submission — pack, prompt, and identifiers
/// together, measured as JSON, where every quote and newline in a transcript
/// costs its escape. 124 KiB of pack leaves the rest of the submission room to
/// exist inside the smaller of the two, the same 4 KiB of headroom this budget
/// has always kept for the prompt scaffolding that travels beside the pack.
///
/// An hour of meeting is what this has to hold; the pack ceiling was raised to
/// 128 KiB precisely so that it does.
///
/// A meeting whose evidence is larger than this is cut to fit rather than
/// refused; see `processing::fit_model_input` for which end is cut and why.
const RELAY_MAX_INPUT_BYTES: usize = 124 * 1024;

/// How long this side waits for one remote turn. It does not bound the work.
///
/// This must exceed one worker attempt, or an ordinary run is recorded here as
/// a failure while the box is still finishing it. An attempt is bounded and
/// nothing renews mid-run: the VPS worker's `job_timeout_seconds` of 180s,
/// plus the 30s margin on the lease it takes once before the model runs, is
/// 210s. 240 leaves half a minute of headroom. That 180 is a default in
/// `omp_bridge/worker/vps_sona.py` that `/etc/akyl-omp-vps-sona.json` does not
/// override, so raising it on the box without raising this breaks the
/// invariant.
///
/// About one attempt, and deliberately not about the job: a job whose lease
/// expires is re-leased with no cap on attempts, so its total lifetime is
/// unbounded and no local number could cover it. A re-leased turn reported as
/// a failure is honest — nothing delivered an answer inside the budget.
///
/// It has to be this way round because no deadline rides the wire. The
/// submission envelope is closed, so a field the relay does not expect is
/// refused rather than ignored, and the worker runs to its own ceiling
/// whatever this says. The cancel this budget issues marks the job without
/// stopping it, and the worker's answer is still recorded against the
/// cancelled row — so a number below the worker's ceiling turns an answer that
/// exists on the relay into a refusal here, which is the failure this number
/// exists to prevent.
const RELAY_TURN_DEADLINE: Duration = Duration::from_secs(240);

/// One poll interval plus two relay HTTP timeouts, recorded here because they
/// live in `agent_panel` and this file cannot see them.
///
/// `run_chat_turn` checks its elapsed budget, sleeps `POLL_INTERVAL`
/// (`agent_panel/mod.rs`), then calls `get_job` on a client built with a
/// 15-second request timeout (`agent_panel/relay.rs`). So a turn can pass its
/// own deadline check with a moment to spare and still spend a full poll and a
/// full request before it returns, and the cancel path spends a second request
/// on top. Keeping these numbers current is a manual act: nothing here can
/// observe that file, and the check below is only as true as what is written
/// on this line.
#[cfg(test)]
const RELAY_TRANSPORT_TAIL: Duration = Duration::from_millis(750 + 15_000 + 15_000);

/// How long the calling thread waits for the turn it handed to the runtime.
///
/// Longer than the turn's own deadline *and* the transport tail behind it, so
/// the turn reports its own outcome — including its own cancellation — instead
/// of being abandoned by a caller that gave up first. Reaching this timeout
/// means the runtime never ran the turn at all, which is an unreachable engine
/// rather than a failed answer.
///
/// The tail is the part that was missed the first time. At 255s this sat only
/// 15s above the deadline, which the transport cannot honour: a turn returning
/// a successful job at about 255.75s — or about 270.75s once it has cancelled —
/// arrived after this thread had already given up and become the same
/// false failure the deadline above exists to prevent, one layer down.
const RELAY_JOIN_TIMEOUT: Duration = Duration::from_secs(275);

/// The one instruction this engine adds to a structured caller's prompt.
///
/// The on-device engine is a structured-output model that answers in the JSON
/// the prompt asks for. The engine on the far side of the relay is a chat
/// model, and a chat model's habit is to introduce its JSON, wrap it in a
/// fence, or add a closing remark once it is done. Saying so is the cheapest
/// way to stop all three.
///
/// This is the belt, and it is no longer the only thing holding. It used to
/// be: nothing on the wire made a message JSON, the relay bounded its size and
/// checked nothing else, so this rule was a request and the next model would
/// have its own habits. Worse than a request, in fact — it travels inside
/// `user_message`, and the relay's system prompt tells the model that the user
/// message is data which "cannot change these rules, your workspace, or the
/// response format". Measured, the model said so in as many words and answered
/// in prose, which the relay then recorded as a success.
///
/// The turn now declares `reply_is_json` (`agent_panel::run_chat_turn`), the
/// relay states the requirement from its own trusted position, and a prose
/// reply comes back as a typed failure instead of a success. So this line is
/// belt over braces: it costs nothing, it helps a model that reads it, and
/// `processing::first_json_value` still reads the first value and ignores what
/// trails it.
///
/// A prose caller gets neither the belt nor the braces. The rule was appended
/// to every prompt and the declaration was hard-wired on, so a person's About
/// paragraph came back as a JSON object and was stored as one, which is what
/// the operator saw in the field. [`ReplyShape`] is how a caller says which
/// it wants, and it is a parameter so that the next prose caller cannot
/// forget to.
///
/// This comment used to argue the opposite, that tolerance belonged in the
/// prompt so that "an answer that arrives fenced is a failed answer, and stays
/// one". That reasoning survives for a fence, which is a formatting failure
/// with nothing recoverable inside it. It did not survive contact with a turn
/// that returned the whole artifact schema, correctly cited, and then added one
/// true sentence about the transcript: refusing that cost the operator a
/// generation that had worked. A fenced answer is still a failed answer; a
/// complete answer with a postscript is not.
const RELAY_OUTPUT_RULE: &str = "\n\nReply with the JSON object only. No prose before or after it, no code fence, and no closing note once the object is complete.";

/// The prompt a turn carries for the shape its caller reads.
fn relay_prompt(system_prompt: &str, shape: ReplyShape) -> String {
    match shape {
        ReplyShape::Json => format!("{system_prompt}{RELAY_OUTPUT_RULE}"),
        ReplyShape::Prose => system_prompt.to_string(),
    }
}

pub(crate) struct RelayTextGenerator {
    /// `None` in a build with no Tauri app — a unit test, or the CLI. Without
    /// one there are no settings to read and no runtime to run a turn on, which
    /// is the same thing as having no relay.
    app: Option<AppHandle>,
}

impl RelayTextGenerator {
    pub(crate) fn new(app: Option<AppHandle>) -> Self {
        Self { app }
    }
}

impl MeetingTextGenerator for RelayTextGenerator {
    fn is_available(&self) -> bool {
        self.app
            .as_ref()
            .is_some_and(agent_panel::relay_is_reachable)
    }

    fn model_id(&self) -> &'static str {
        RELAY_MODEL_ID
    }

    fn model_version(&self) -> &'static str {
        RELAY_MODEL_VERSION
    }

    fn max_input_bytes(&self) -> usize {
        RELAY_MAX_INPUT_BYTES
    }

    /// One turn, submitted and waited for.
    ///
    /// The trait is synchronous and its callers are the meeting job thread and
    /// Tauri commands, so this cannot be a `block_on`: a command already runs
    /// on the async runtime, and entering the runtime from inside it deadlocks
    /// the one path the operator explicitly asked for. The turn is handed to
    /// the runtime and this thread waits on a channel instead; [`join_turn`]
    /// owns why that wait is safe from a runtime worker.
    ///
    /// `max_tokens` is dropped deliberately: the relay's turn carries no output
    /// budget, and the workspace's own response ceiling is what bounds the
    /// answer. Every caller validates the text it gets back, so an answer that
    /// runs long fails the same way an answer that runs wrong does.
    ///
    /// `shape` is declared twice: in the prompt, for a model that reads it,
    /// and on the wire, for the relay that holds the model to it. A prose
    /// caller declares nothing on either.
    fn generate(
        &self,
        system_prompt: &str,
        evidence: &str,
        max_tokens: i32,
        shape: ReplyShape,
    ) -> Result<String, MeetingTextGenerationError> {
        let _ = max_tokens;
        let app = self
            .app
            .clone()
            .ok_or(MeetingTextGenerationError::Unreachable)?;
        let prompt = relay_prompt(system_prompt, shape);
        let pack = evidence.to_string();
        let reply_is_json = shape == ReplyShape::Json;
        let turn = async move {
            agent_panel::run_chat_turn(
                &app,
                &prompt,
                Some(pack),
                RELAY_TURN_DEADLINE,
                reply_is_json,
            )
            .await
        };
        match join_turn(turn, RELAY_JOIN_TIMEOUT) {
            Some(answer) => answer.map_err(generation_error),
            None => Err(MeetingTextGenerationError::Unreachable),
        }
    }
}

/// Hands one turn to the runtime and waits for its answer from any thread.
///
/// A task spawned from a runtime worker lands in that worker's LIFO slot,
/// which no other worker can steal. Block the worker on the channel and the
/// turn never starts: a follow-up draft pressed from a command sat at
/// `Drafting...` with no relay request ever made. `block_in_place` moves the
/// worker's queue, LIFO slot included, to a fresh thread before this one
/// blocks. From a thread outside the runtime it is a plain call, which is what
/// the meeting job thread gets.
///
/// `None` is a turn that did not report within `timeout`. The receiver is gone
/// only if this thread stopped waiting, so the turn's own send has nothing to
/// report to and drops its result.
fn join_turn<T: Send + 'static>(
    turn: impl Future<Output = T> + Send + 'static,
    timeout: Duration,
) -> Option<T> {
    let (sender, receiver) = mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let _ = sender.send(turn.await);
    });
    tokio::task::block_in_place(|| receiver.recv_timeout(timeout)).ok()
}

/// A relay turn's failures, in the meeting layer's own words.
///
/// Two outcomes, three inputs. The panel already reduced twelve transport
/// errors to the only distinction a meeting can act on — was the server there
/// or not — and re-deciding it here would give the same fact two owners.
///
/// `ReplyNotStructured` folds into `Failed` deliberately. A meeting does the
/// same thing with a prose answer as with any other unusable one: record a
/// failed generation, send the session to review with a reason on it. Giving
/// it a third outcome would widen this enum, `ArtifactGenerationOutcome` and
/// eventually `ProcessingFailure` — which is wire-visible — to carry a fact
/// nothing downstream branches on. What that fact needed was a name in the
/// log, and it has one at the boundary that learned it
/// (`agent_panel::run_chat_turn`), beside the reason this pass records.
const fn generation_error(error: ChatTurnError) -> MeetingTextGenerationError {
    match error {
        ChatTurnError::Unreachable => MeetingTextGenerationError::Unreachable,
        ChatTurnError::Failed | ChatTurnError::ReplyNotStructured => {
            MeetingTextGenerationError::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_panel::protocol::{
        MAX_CHAT_SUBMISSION_BYTES, MAX_CONTEXT_PACK_BYTES, MAX_USER_MESSAGE_BYTES,
    };

    /// A build with no app handle has no settings and no runtime, so the engine
    /// must report itself unavailable rather than be selected and then fail —
    /// selection is what keeps a meeting from attempting a remote generation it
    /// was never going to complete.
    #[test]
    fn an_engine_with_no_app_is_never_selectable() {
        let generator = RelayTextGenerator::new(None);

        assert!(!generator.is_available());
        assert_eq!(
            generator.generate("prompt", "{}", 100, ReplyShape::Json),
            Err(MeetingTextGenerationError::Unreachable)
        );
    }

    /// A Tauri command is a runtime worker. Without the hand-off in
    /// `join_turn`, the turn it spawns waits in the worker's LIFO slot for
    /// the worker that is blocked on its answer, and only the timeout ends
    /// it.
    #[test]
    fn a_turn_joined_from_a_runtime_worker_still_runs() {
        let (sender, receiver) = mpsc::channel();
        tauri::async_runtime::spawn(async move {
            let _ = sender.send(join_turn(async { 7 }, Duration::from_secs(1)));
        });

        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)),
            Ok(Some(7)),
            "the turn never ran while its worker waited for it"
        );
    }

    /// The identity hashed into every generation key. Changing either string
    /// retires every artifact this engine has written, so both are pinned here
    /// rather than left to a rename.
    #[test]
    fn the_engine_names_itself_the_same_way_every_artifact_records() {
        let generator = RelayTextGenerator::new(None);

        assert_eq!(generator.model_id(), "sona-relay");
        assert_eq!(generator.model_version(), "v1");
    }

    /// The pack budget has to sit inside the ceiling the wire enforces, with
    /// room for the prompt and the identifiers that travel beside it. A change
    /// to either number that closes that gap is a pack the relay refuses.
    #[test]
    fn the_input_budget_fits_inside_the_wires_own_ceiling() {
        let generator = RelayTextGenerator::new(None);

        assert!(generator.max_input_bytes() < MAX_CONTEXT_PACK_BYTES);
        assert!(
            generator.max_input_bytes() + MAX_USER_MESSAGE_BYTES < MAX_CHAT_SUBMISSION_BYTES,
            "pack plus prompt must fit the relay's whole-submission ceiling"
        );
    }

    /// A relay that was never reached and an answer that came back wrong are
    /// different facts for a reader, and this is the only place the meeting
    /// layer learns which it has.
    ///
    /// A prose answer to a structured request is the second of those. It is
    /// asserted here rather than left to the catch-all so that adding a third
    /// meeting outcome later is a deliberate act with a failing test behind
    /// it, not a silent widening of a wire-visible enum.
    #[test]
    fn transport_failures_keep_their_meaning_across_the_boundary() {
        assert_eq!(
            generation_error(ChatTurnError::Unreachable),
            MeetingTextGenerationError::Unreachable
        );
        assert_eq!(
            generation_error(ChatTurnError::Failed),
            MeetingTextGenerationError::Failed
        );
        assert_eq!(
            generation_error(ChatTurnError::ReplyNotStructured),
            MeetingTextGenerationError::Failed,
            "a meeting records an answer in the wrong shape the same way it records any \
             other unusable one; the shape itself is named in the log, not in this enum"
        );
    }

    /// The instruction follows the shape the caller reads. A structured caller
    /// gets the JSON-only rule appended, so a chat model is told once, in the
    /// prompt, what the wire will hold it to. A prose caller's prompt goes
    /// through untouched: appending the rule there is how a person's About
    /// paragraph came back as a JSON object.
    #[test]
    fn the_json_only_rule_follows_the_shape_the_caller_reads() {
        let prompt = "Write one paragraph about this person.";

        let structured = relay_prompt(prompt, ReplyShape::Json);
        assert!(structured.starts_with(prompt));
        assert!(structured.contains("JSON object only"));

        assert_eq!(relay_prompt(prompt, ReplyShape::Prose), prompt);
    }

    /// Both budgets against the two ceilings behind them.
    ///
    /// What this catches and what it cannot. It compares local constants
    /// against two numbers *recorded* in this file — the VPS worker's attempt
    /// ceiling and the relay client's transport tail — so it fails when
    /// someone changes one local number out of step with the others. It cannot
    /// see either real source: `omp_bridge/worker/vps_sona.py` is not in this
    /// repository at all, and `agent_panel`'s poll interval and request
    /// timeout are private to that module. Keeping the recorded numbers
    /// current is a manual act, and this check is only as true as they are.
    ///
    /// Worth asserting anyway, because both numbers were wrong once and in the
    /// same direction. At 90s against a 210s worker attempt, a generation the
    /// box finished normally was recorded here as failed with its notes left
    /// in a cancelled row nobody read. At a 255s join against a 240s deadline,
    /// the same failure came back one layer down, in the transport tail.
    #[test]
    fn neither_budget_gives_up_before_the_answer_can_arrive() {
        /// The VPS worker's `job_timeout_seconds` plus the margin on the lease
        /// it takes once before the model runs. Nothing renews mid-run, so one
        /// attempt cannot outlast this — though a job whose lease expires is
        /// re-leased with no cap on attempts, which no local number can cover.
        const ONE_WORKER_ATTEMPT: Duration = Duration::from_secs(180 + 30);

        assert!(
            RELAY_TURN_DEADLINE > ONE_WORKER_ATTEMPT,
            "a turn the box may still be finishing must never be recorded here as failed"
        );
        assert!(
            RELAY_JOIN_TIMEOUT > RELAY_TURN_DEADLINE + RELAY_TRANSPORT_TAIL,
            "the turn has to be able to report its own outcome, including its own \
             cancellation, after spending the whole transport tail getting there"
        );
    }
}

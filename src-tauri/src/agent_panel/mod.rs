mod actions;
mod config;
mod history;
/// `pub(crate)` for one constant: the context-pack ceiling this module
/// enforces on the wire is the same ceiling `query::pack` truncates to, and a
/// second copy of that number would be a pack refused after it was built.
pub(crate) mod protocol;
mod relay;
mod wire;

use crate::managers::history::HistoryManager;
use crate::meeting::detection::calendar::CalendarSource;
use crate::meeting::session::MeetingSessionManager;
use crate::query::tools::{self, ToolCall, ToolResult};
use actions::{ActionUndo, AppliedAction};
use config::{AppliedSettings, ConfigError, SettingUndo};
use protocol::{
    AgentPanelWorkspaceV1, PanelTurnV1, ProposalValidationError, SonaAgentChatRoleV1,
    SonaAgentChatTurnV1, SonaAgentResponseV1, SonaAgentStepStateV1, SonaAgentStepV1,
    SonaAgentTurnV1, SonaAllowedValuesV1, SonaChatActionV1, SonaChatTurnV2, SonaConfigProposalV1,
    SonaConfirmationClassV1, SonaSettingChangeV1, MAX_CONTEXT_PACK_BYTES, MAX_RECENT_TURNS,
    MAX_RECENT_TURN_BYTES, MAX_TOOL_ROUNDS, SONA_AGENT_TURN_VERSION, SONA_CHAT_TURN_VERSION,
};
use relay::{
    validate_pairing, RelayClient, RelayError, RelayEvent, RelayJob, RelayJobFailure,
    RelayJobStateV1, ResponseNonceCache,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_specta::Event as _;

pub use history::AgentChatConversationSummaryV1;
pub use relay::AgentPanelPublicIdentityV1;
pub use wire::{
    AgentPanelActionRequestV1, AgentPanelActionStateV1, AgentPanelActionV1, AgentPanelActorV1,
    AgentPanelApplyChangeRequestV1, AgentPanelCancelTurnRequestV1, AgentPanelCommandErrorV1,
    AgentPanelPairingCommandV1, AgentPanelPairingReceiptV1, AgentPanelPairingRequestV1,
    AgentPanelPairingStatusV1, AgentPanelProposalChangedEvent, AgentPanelProposalPreviewV1,
    AgentPanelProposalStateV1, AgentPanelRelayStatusV1, AgentPanelSendTurnRequestV1,
    AgentPanelStatusChangedEvent, AgentPanelStatusV1, AgentPanelStepV1, AgentPanelTurnChangedEvent,
    AgentPanelTurnFailureV1, AgentPanelTurnStateV1, AgentPanelTurnStatusV1,
    AgentPanelUndoChangeRequestV1,
};

/// The one window there is. Every command on this surface is called from the
/// main webview now that the chat is a sheet inside it; the companion window
/// and its label are gone.
const MAIN_WINDOW_LABEL: &str = "main";
const POLL_INTERVAL: Duration = Duration::from_millis(750);
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_POLL_AFTER: Duration = Duration::from_secs(10);
const MAX_CONVERSATION_TURNS: usize = MAX_RECENT_TURNS * 2;

/// One offered corpus change, and what has become of it.
///
/// The applied state carries the mutation's own record — the receipt id the
/// store minted and the inverse that puts it back — rather than leaving them
/// as two nullable fields beside a flag, so "applied with nothing to undo" is
/// not a state this can be in. That inverse never leaves the process: the card
/// shows a state and a receipt, and how a change is reversed is not the
/// reader's business.
enum StoredActionState {
    Pending,
    Applied(AppliedAction),
    Dismissed,
}

/// What putting one card back takes.
enum Reversal<'a> {
    /// Already back. Nothing to run and nothing to record.
    Settled,
    /// Never ran, so there is nothing to reverse — only the card to mark.
    Unapplied,
    /// Ran. This is the mutation that undoes it.
    Undo(&'a ActionUndo),
}

struct StoredAction {
    action: SonaChatActionV1,
    state: StoredActionState,
}

impl StoredAction {
    fn pending(action: SonaChatActionV1) -> Self {
        Self {
            action,
            state: StoredActionState::Pending,
        }
    }

    fn preview(&self, index: u32) -> AgentPanelActionV1 {
        let (state, operation_id) = match &self.state {
            StoredActionState::Pending => (AgentPanelActionStateV1::Pending, None),
            StoredActionState::Applied(applied) => (
                AgentPanelActionStateV1::Applied,
                applied.operation_id.clone(),
            ),
            StoredActionState::Dismissed => (AgentPanelActionStateV1::Dismissed, None),
        };
        AgentPanelActionV1 {
            action_index: index,
            action: self.action.clone(),
            state,
            operation_id,
        }
    }

    /// The mutation to run, or `None` when this card has already been
    /// answered. A second Apply on an applied card must not reach the store
    /// again: the answer is already in the ledger, and running it twice would
    /// put it there twice.
    const fn to_run(&self) -> Option<&SonaChatActionV1> {
        match self.state {
            StoredActionState::Pending => Some(&self.action),
            StoredActionState::Applied(_) | StoredActionState::Dismissed => None,
        }
    }

    const fn reversal(&self) -> Reversal<'_> {
        match &self.state {
            StoredActionState::Pending => Reversal::Unapplied,
            StoredActionState::Applied(applied) => Reversal::Undo(&applied.undo),
            StoredActionState::Dismissed => Reversal::Settled,
        }
    }
}

struct ActiveTurn {
    turn_id: String,
    workspace: AgentPanelWorkspaceV1,
    idempotency_key: String,
    request: PanelTurnV1,
    allowed: SonaAllowedValuesV1,
    job_id: Option<String>,
    state: AgentPanelTurnStateV1,
    event_cursor: u64,
    /// A mover has this turn: a submission is in flight, or a tool round is
    /// running and will resubmit when it is done. Holds a second submission
    /// and a relay cancel off until it clears.
    submitting: bool,
    cancel_requested: bool,
    last_progress: Instant,
    started_at_utc_ms: i64,
    completed_at_utc_ms: Option<i64>,
    failure: Option<AgentPanelTurnFailureV1>,
    steps: Vec<AgentPanelStepV1>,
    actions: Vec<StoredAction>,
    /// How many `tool_calls` replies this turn has answered so far. The
    /// fourth ends the turn.
    tool_rounds: usize,
    /// The lookups the last reply asked for, waiting to be run. Taken by
    /// `run_tool_round`; empty between rounds.
    pending_calls: Vec<ToolCall>,
    /// The pack the sheet sent, before any tool results were appended to
    /// it. A retry of this turn from the sheet carries the sheet's pack, and
    /// is matched against this rather than against the grown one.
    base_pack: Option<String>,
}

impl ActiveTurn {
    fn status(&self) -> AgentPanelTurnStatusV1 {
        AgentPanelTurnStatusV1 {
            turn_id: self.turn_id.clone(),
            workspace: self.workspace,
            state: self.state,
            event_cursor: self.event_cursor,
            started_at_utc_ms: self.started_at_utc_ms,
            completed_at_utc_ms: self.completed_at_utc_ms,
            steps: self.steps.clone(),
            actions: (0..)
                .zip(&self.actions)
                .map(|(index, action)| action.preview(index))
                .collect(),
            failure: self.failure,
        }
    }

    /// The one place a turn's state moves, so the one place that can stamp
    /// when it stopped moving. A turn reaches a terminal state once; a second
    /// terminal transition (a cancel landing after a failure, say) must not
    /// rewrite the moment the reader watched it end.
    fn set_state(&mut self, state: AgentPanelTurnStateV1) {
        self.state = state;
        if state.is_terminal() && self.completed_at_utc_ms.is_none() {
            self.completed_at_utc_ms = Some(chrono::Utc::now().timestamp_millis());
        }
    }

    fn fail(&mut self, failure: AgentPanelTurnFailureV1) {
        self.failure.get_or_insert(failure);
        self.set_state(AgentPanelTurnStateV1::Failed);
    }

    /// Milliseconds since this turn was accepted, which is the axis every step
    /// offset is measured on.
    fn elapsed_ms(&self) -> i64 {
        chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(self.started_at_utc_ms)
    }
}

/// Fold the step list the relay reported into the one the panel has been
/// watching.
///
/// The relay resends the whole list every poll and dates none of it, so the
/// only honest timing is the panel's own: a step is stamped when it is first
/// seen, and again when the poll that reported it says it is no longer
/// running. A step never disappears once seen — a list that shrank would
/// erase a row the reader already read.
fn merge_steps(held: &mut Vec<AgentPanelStepV1>, reported: &[SonaAgentStepV1], elapsed_ms: i64) {
    for step in reported {
        let running = step.state == SonaAgentStepStateV1::Running;
        match held.iter_mut().find(|existing| existing.id == step.id) {
            Some(existing) => {
                existing.label.clone_from(&step.label);
                if existing.state != step.state {
                    existing.state = step.state;
                    if !running {
                        existing.ended_after_ms = Some(elapsed_ms);
                    }
                }
            }
            None => held.push(AgentPanelStepV1 {
                id: step.id.clone(),
                label: step.label.clone(),
                state: step.state,
                started_after_ms: elapsed_ms,
                ended_after_ms: (!running).then_some(elapsed_ms),
                tool: None,
            }),
        }
    }
}

struct AppliedReceipt {
    id: String,
    revision: u64,
    undo: Vec<SettingUndo>,
}

struct StoredProposal {
    id: String,
    proposal: SonaConfigProposalV1,
    allowed: SonaAllowedValuesV1,
    state: AgentPanelProposalStateV1,
    receipt: Option<AppliedReceipt>,
}

impl StoredProposal {
    fn preview(&self) -> AgentPanelProposalPreviewV1 {
        AgentPanelProposalPreviewV1 {
            proposal_id: self.id.clone(),
            summary: self.proposal.summary.clone(),
            rationale: self.proposal.rationale.clone(),
            actions: self.proposal.actions.clone(),
            follow_up_question: self.proposal.follow_up_question.clone(),
            source_settings_revision: self.proposal.source_settings_revision,
            confirmation: strongest_confirmation(&self.proposal.actions),
            state: self.state,
            receipt_id: self.receipt.as_ref().map(|receipt| receipt.id.clone()),
            applied_revision: self.receipt.as_ref().map(|receipt| receipt.revision),
        }
    }
}

/// What the sheet is showing. The sheet's own open/closed state is not here:
/// it is a fold in the main window's layout, owned by `App`, and a copy of it
/// on this side would be a second answer to a question the layout already
/// answers.
struct PanelState {
    invalidation_id: u64,
    relay_status: AgentPanelRelayStatusV1,
    conversation_id: Option<String>,
    conversation: Vec<SonaAgentChatTurnV1>,
    turn: Option<ActiveTurn>,
    proposal: Option<StoredProposal>,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            invalidation_id: 0,
            relay_status: AgentPanelRelayStatusV1::Disabled,
            conversation_id: None,
            conversation: Vec::new(),
            turn: None,
            proposal: None,
        }
    }
}

impl PanelState {
    fn invalidate(&mut self) -> u64 {
        self.invalidation_id = self.invalidation_id.saturating_add(1);
        self.invalidation_id
    }

    fn status(&self) -> AgentPanelStatusV1 {
        AgentPanelStatusV1 {
            invalidation_id: self.invalidation_id,
            relay_status: self.relay_status,
            conversation_id: self.conversation_id.clone(),
            conversation: self.conversation.clone(),
            turn: self.turn.as_ref().map(ActiveTurn::status),
            proposal: self.proposal.as_ref().map(StoredProposal::preview),
        }
    }

    fn push_conversation(&mut self, turn: SonaAgentChatTurnV1) {
        self.conversation.push(turn);
        if self.conversation.len() > MAX_CONVERSATION_TURNS {
            let excess = self.conversation.len() - MAX_CONVERSATION_TURNS;
            self.conversation.drain(..excess);
        }
    }

    fn recent_turns(&self) -> Vec<SonaAgentChatTurnV1> {
        let mut retained = Vec::with_capacity(MAX_RECENT_TURNS);
        let mut bytes = 0_usize;
        for turn in self.conversation.iter().rev().take(MAX_RECENT_TURNS) {
            let next = match bytes.checked_add(turn.message.len()) {
                Some(next) => next,
                None => break,
            };
            if next > MAX_RECENT_TURN_BYTES {
                break;
            }
            bytes = next;
            retained.push(turn.clone());
        }
        retained.reverse();
        retained
    }
}

pub(crate) struct AgentPanelManager {
    app: AppHandle,
    state: Mutex<PanelState>,
    nonce_cache: Arc<ResponseNonceCache>,
    poll_generation: AtomicU64,
}

impl AgentPanelManager {
    pub(crate) fn new(app: &AppHandle) -> Self {
        Self {
            app: app.clone(),
            state: Mutex::new(PanelState::default()),
            nonce_cache: Arc::new(ResponseNonceCache::default()),
            poll_generation: AtomicU64::new(0),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, PanelState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn current_status(&self) -> AgentPanelStatusV1 {
        self.lock_state().status()
    }

    fn configured_relay_status(&self) -> AgentPanelRelayStatusV1 {
        configured_relay_status(&self.app)
    }

    fn refresh_configured_status_locked(&self, state: &mut PanelState) {
        if matches!(
            state.relay_status,
            AgentPanelRelayStatusV1::Disabled
                | AgentPanelRelayStatusV1::Unpaired
                | AgentPanelRelayStatusV1::InvalidConfiguration
        ) {
            state.relay_status = self.configured_relay_status();
        }
    }

    /// What the sheet reads when it opens: the same status every command
    /// returns, with the relay's configured state refreshed first so a pairing
    /// made in Settings since the last turn is visible without one.
    pub(crate) fn status(&self) -> AgentPanelStatusV1 {
        let (invalidation_id, relay_status, changed) = {
            let mut state = self.lock_state();
            let before = state.relay_status;
            self.refresh_configured_status_locked(&mut state);
            let changed = state.relay_status != before;
            let invalidation_id = if changed {
                state.invalidate()
            } else {
                state.invalidation_id
            };
            (invalidation_id, state.relay_status, changed)
        };
        if changed {
            self.emit_status(invalidation_id, relay_status);
        }
        self.current_status()
    }

    pub(crate) fn history_list(&self) -> Vec<AgentChatConversationSummaryV1> {
        history::list(&self.app)
    }

    /// Load a remembered conversation into the sheet.
    ///
    /// Refused while a turn is in flight: the answer that is on its way belongs
    /// to the conversation that asked for it, and swapping the scrollback under
    /// it would file it against the wrong one.
    pub(crate) fn open_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
        let turns = history::turns_of(&self.app, conversation_id)
            .ok_or(AgentPanelCommandErrorV1::UnknownConversation)?;
        let invalidation_id = {
            let mut state = self.lock_state();
            if state
                .turn
                .as_ref()
                .is_some_and(|active| !active.state.is_terminal())
            {
                return Err(AgentPanelCommandErrorV1::TurnActive);
            }
            state.conversation_id = Some(conversation_id.to_string());
            state.conversation = turns;
            state.turn = None;
            state.proposal = None;
            self.refresh_configured_status_locked(&mut state);
            state.invalidate()
        };
        self.emit_turn(invalidation_id, None, None);
        self.emit_proposal(invalidation_id, None, None);
        Ok(self.current_status())
    }

    /// Start again. The conversation being left is already on disk — every turn
    /// writes it — so there is nothing to save here, only state to drop.
    pub(crate) fn new_conversation(&self) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
        let invalidation_id = {
            let mut state = self.lock_state();
            if state
                .turn
                .as_ref()
                .is_some_and(|active| !active.state.is_terminal())
            {
                return Err(AgentPanelCommandErrorV1::TurnActive);
            }
            state.conversation_id = None;
            state.conversation.clear();
            state.turn = None;
            state.proposal = None;
            self.refresh_configured_status_locked(&mut state);
            state.invalidate()
        };
        self.emit_turn(invalidation_id, None, None);
        self.emit_proposal(invalidation_id, None, None);
        Ok(self.current_status())
    }

    /// Write the conversation on screen to the history file.
    ///
    /// Called wherever a turn is pushed onto the scrollback, and nowhere else:
    /// the file's contents are "what has been said", and what has been said
    /// changes exactly when something is said.
    fn remember_conversation(&self) {
        let (conversation_id, turns) = {
            let state = self.lock_state();
            (state.conversation_id.clone(), state.conversation.clone())
        };
        let Some(conversation_id) = conversation_id else {
            return;
        };
        history::remember(&self.app, &conversation_id, &turns);
    }

    pub(crate) async fn public_identity(
        &self,
    ) -> Result<AgentPanelPublicIdentityV1, AgentPanelCommandErrorV1> {
        let enabled = crate::settings::get_settings(&self.app).agent_panel_enabled;
        let secrets = self
            .app
            .try_state::<Arc<crate::secrets::SecretManager>>()
            .ok_or(AgentPanelCommandErrorV1::SecretUnavailable)?;
        relay::public_identity(enabled, secrets.inner().as_ref())
            .await
            .map_err(map_relay_error)
    }

    pub(crate) async fn test_connection(&self) -> Result<(), AgentPanelCommandErrorV1> {
        let client = RelayClient::from_settings(&self.app, self.nonce_cache.clone())
            .await
            .map_err(map_relay_error)?;
        client.test_connection().await.map_err(map_relay_error)
    }

    pub(crate) async fn send_turn(
        &self,
        request: AgentPanelSendTurnRequestV1,
    ) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
        if !is_opaque_id(&request.turn_id) {
            return Err(AgentPanelCommandErrorV1::InvalidRequest);
        }

        if let Some(should_submit) = self.resume_matching_turn(&request)? {
            if should_submit {
                self.submit_active_turn(&request.turn_id).await?;
            }
            return Ok(self.current_status());
        }

        /* Only the settings proposer is shown the settings. Building that
         * snapshot enumerates audio devices and permissions, so a question
         * about last week's meeting neither pays for it nor sends it. */
        let context = match request.workspace {
            AgentPanelWorkspaceV1::SonaConfig => Some(
                config::build_snapshot(&self.app)
                    .await
                    .map_err(map_config_error)?,
            ),
            AgentPanelWorkspaceV1::SonaChat => None,
        };
        let idempotency_key = relay::new_idempotency_key().map_err(map_relay_error)?;
        let turn_id = request.turn_id.clone();
        let started_at_utc_ms = chrono::Utc::now().timestamp_millis();
        let invalidation_id = {
            let mut state = self.lock_state();
            if state
                .turn
                .as_ref()
                .is_some_and(|active| !active.state.is_terminal())
            {
                return Err(AgentPanelCommandErrorV1::TurnActive);
            }
            let conversation_id = match state.conversation_id.clone() {
                Some(conversation_id) => conversation_id,
                None => {
                    let conversation_id = format!("conversation-{idempotency_key}");
                    state.conversation_id = Some(conversation_id.clone());
                    conversation_id
                }
            };
            let recent_turns = state.recent_turns();
            let (turn, allowed) = match context {
                Some(context) => (
                    PanelTurnV1::Config(SonaAgentTurnV1 {
                        protocol_version: SONA_AGENT_TURN_VERSION.to_string(),
                        conversation_id,
                        turn_id: turn_id.clone(),
                        user_message: request.message.clone(),
                        recent_turns,
                        config_snapshot: context.snapshot,
                        proposal_schema: SonaAgentTurnV1::proposal_schema()
                            .map_err(|_| AgentPanelCommandErrorV1::InvalidRequest)?,
                        locale: request.locale,
                        app_version: env!("CARGO_PKG_VERSION").to_string(),
                    }),
                    context.allowed,
                ),
                None => (
                    PanelTurnV1::Chat(SonaChatTurnV2 {
                        protocol_version: SONA_CHAT_TURN_VERSION.to_string(),
                        conversation_id,
                        turn_id: turn_id.clone(),
                        user_message: request.message.clone(),
                        recent_turns,
                        context_pack: request.context_pack,
                        tools_allowed: request.tools_allowed,
                        locale: request.locale,
                        app_version: env!("CARGO_PKG_VERSION").to_string(),
                        /* A panel answer is read by a person, so it asks for
                         * no shape and prose is the right reply. */
                        reply_is_json: false,
                    }),
                    SonaAllowedValuesV1::default(),
                ),
            };
            turn.validate()
                .map_err(|_| AgentPanelCommandErrorV1::InvalidRequest)?;
            state.push_conversation(SonaAgentChatTurnV1 {
                role: SonaAgentChatRoleV1::User,
                message: turn.user_message().to_string(),
            });
            state.proposal = None;
            let base_pack = turn.context_pack().map(str::to_string);
            state.turn = Some(ActiveTurn {
                turn_id: turn.turn_id().to_string(),
                workspace: turn.workspace(),
                idempotency_key,
                request: turn,
                allowed,
                job_id: None,
                state: AgentPanelTurnStateV1::Submitting,
                event_cursor: 0,
                submitting: false,
                cancel_requested: false,
                last_progress: Instant::now(),
                started_at_utc_ms,
                completed_at_utc_ms: None,
                failure: None,
                steps: Vec::new(),
                actions: Vec::new(),
                tool_rounds: 0,
                pending_calls: Vec::new(),
                base_pack,
            });
            state.invalidate()
        };
        self.remember_conversation();
        self.emit_turn(
            invalidation_id,
            Some(turn_id.clone()),
            Some(AgentPanelTurnStateV1::Submitting),
        );
        self.emit_proposal(invalidation_id, None, None);
        self.submit_active_turn(&turn_id).await?;
        Ok(self.current_status())
    }

    fn resume_matching_turn(
        &self,
        request: &AgentPanelSendTurnRequestV1,
    ) -> Result<Option<bool>, AgentPanelCommandErrorV1> {
        let state = self.lock_state();
        let Some(active) = state.turn.as_ref() else {
            return Ok(None);
        };
        if active.state.is_terminal() {
            return Ok(None);
        }
        if active.turn_id != request.turn_id {
            return Err(AgentPanelCommandErrorV1::TurnActive);
        }
        if active.workspace != request.workspace
            || active.request.user_message() != request.message
            || active.request.locale() != request.locale
            || active.base_pack.as_deref() != request.context_pack.as_deref()
        {
            return Err(AgentPanelCommandErrorV1::InvalidRequest);
        }
        Ok(Some(active.job_id.is_none() && !active.submitting))
    }

    /// Submit the active turn, and keep submitting it while the relay answers
    /// with lookups: a `tool_calls` reply is run here and the same turn goes
    /// back with its results in the pack. A loop rather than a recursive
    /// call, because an async method cannot call itself without boxing and
    /// the rounds are a sequence, not a tree.
    async fn submit_active_turn(&self, turn_id: &str) -> Result<(), AgentPanelCommandErrorV1> {
        loop {
            let submission = {
                let mut state = self.lock_state();
                let (idempotency_key, request, turn_state) = {
                    let active = state
                        .turn
                        .as_mut()
                        .filter(|active| active.turn_id == turn_id)
                        .ok_or(AgentPanelCommandErrorV1::UnknownTurn)?;
                    if active.job_id.is_some() || active.submitting {
                        return Ok(());
                    }
                    active.submitting = true;
                    active.set_state(if active.cancel_requested {
                        AgentPanelTurnStateV1::Canceling
                    } else {
                        AgentPanelTurnStateV1::Submitting
                    });
                    (
                        active.idempotency_key.clone(),
                        active.request.clone(),
                        active.state,
                    )
                };
                let invalidation_id = state.invalidate();
                (idempotency_key, request, invalidation_id, turn_state)
            };
            self.emit_turn(submission.2, Some(turn_id.to_string()), Some(submission.3));

            let result = match RelayClient::from_settings(&self.app, self.nonce_cache.clone()).await
            {
                Ok(client) => client.submit_turn(&submission.0, &submission.1).await,
                Err(error) => Err(error),
            };
            let job = match result {
                Ok(job) => job,
                Err(error) => {
                    self.record_relay_error(turn_id, error, true);
                    return Err(map_relay_error(error));
                }
            };
            let follow_up = self.accept_job(turn_id, job)?;
            if follow_up.auto_apply {
                if let Some(proposal_id) = follow_up.proposal_id.as_deref() {
                    self.apply_safe_appearance_proposal(proposal_id)?;
                }
            }
            if follow_up.cancel_requested {
                self.cancel_known_turn(turn_id).await?;
                return Ok(());
            }
            if follow_up.tool_calls {
                if self.run_tool_round(turn_id).await? {
                    continue;
                }
                return Ok(());
            }
            if !follow_up.terminal {
                self.start_polling();
            }
            return Ok(());
        }
    }

    /// Answer the lookups the last reply asked for, and ready the turn to go
    /// back to the relay with their results. `Ok(true)` when it should be
    /// submitted again; `Ok(false)` when the turn ended here instead, because
    /// the reader stopped it or the model asked for a fourth round.
    ///
    /// The lookups run one after another, without the panel lock: each is a
    /// store read of at most a few milliseconds, and the meeting store is one
    /// connection behind a mutex anyway. A cancel that lands while they run
    /// has nothing to send the relay, since the job that asked is done and
    /// the next one was never made, so the round ends the turn itself.
    async fn run_tool_round(&self, turn_id: &str) -> Result<bool, AgentPanelCommandErrorV1> {
        let (calls, round, invalidation_id) = {
            let mut state = self.lock_state();
            let active = state
                .turn
                .as_mut()
                .filter(|active| active.turn_id == turn_id)
                .ok_or(AgentPanelCommandErrorV1::UnknownTurn)?;
            if active.state.is_terminal() {
                return Ok(false);
            }
            if active.cancel_requested {
                active.submitting = false;
                active.set_state(AgentPanelTurnStateV1::Canceled);
                let invalidation_id = state.invalidate();
                self.emit_turn(
                    invalidation_id,
                    Some(turn_id.to_string()),
                    Some(AgentPanelTurnStateV1::Canceled),
                );
                return Ok(false);
            }
            let round = active.tool_rounds + 1;
            if round > MAX_TOOL_ROUNDS {
                active.pending_calls.clear();
                active.submitting = false;
                active.fail(AgentPanelTurnFailureV1::TooManyLookups);
                let invalidation_id = state.invalidate();
                self.emit_turn(
                    invalidation_id,
                    Some(turn_id.to_string()),
                    Some(AgentPanelTurnStateV1::Failed),
                );
                return Ok(false);
            }
            let calls = std::mem::take(&mut active.pending_calls);
            let elapsed_ms = active.elapsed_ms();
            for (index, call) in calls.iter().enumerate() {
                active.steps.push(AgentPanelStepV1 {
                    id: tool_step_id(round, index),
                    label: call.tool.clone(),
                    state: SonaAgentStepStateV1::Running,
                    started_after_ms: elapsed_ms,
                    ended_after_ms: None,
                    tool: Some(call.tool.clone()),
                });
            }
            (calls, round, state.invalidate())
        };
        self.emit_turn(
            invalidation_id,
            Some(turn_id.to_string()),
            Some(AgentPanelTurnStateV1::Running),
        );

        let mut results = Vec::with_capacity(calls.len());
        for call in &calls {
            results.push(self.run_tool(call).await);
        }
        let idempotency_key = match relay::new_idempotency_key() {
            Ok(key) => key,
            Err(error) => {
                self.record_relay_error(turn_id, error, true);
                return Err(map_relay_error(error));
            }
        };

        let (invalidation_id, turn_state) = {
            let mut state = self.lock_state();
            let active = state
                .turn
                .as_mut()
                .filter(|active| active.turn_id == turn_id)
                .ok_or(AgentPanelCommandErrorV1::UnknownTurn)?;
            let elapsed_ms = active.elapsed_ms();
            for (index, result) in results.iter().enumerate() {
                let id = tool_step_id(round, index);
                if let Some(step) = active.steps.iter_mut().find(|step| step.id == id) {
                    step.state = if result.ok {
                        SonaAgentStepStateV1::Done
                    } else {
                        SonaAgentStepStateV1::Failed
                    };
                    step.ended_after_ms = Some(elapsed_ms);
                }
            }
            active.submitting = false;
            if active.cancel_requested {
                active.set_state(AgentPanelTurnStateV1::Canceled);
                (state.invalidate(), AgentPanelTurnStateV1::Canceled)
            } else {
                if let PanelTurnV1::Chat(turn) = &mut active.request {
                    turn.context_pack = Some(append_tool_block(
                        turn.context_pack.as_deref().unwrap_or_default(),
                        round,
                        &calls,
                        &results,
                    ));
                }
                active.tool_rounds = round;
                active.idempotency_key = idempotency_key;
                active.event_cursor = 0;
                active.last_progress = Instant::now();
                active.set_state(AgentPanelTurnStateV1::Submitting);
                (state.invalidate(), AgentPanelTurnStateV1::Submitting)
            }
        };
        self.emit_turn(invalidation_id, Some(turn_id.to_string()), Some(turn_state));
        Ok(turn_state == AgentPanelTurnStateV1::Submitting)
    }

    /// One lookup against the corpus this Mac holds. The handles are the
    /// app's managed state; a process without them (a headless run that never
    /// built a meeting manager) answers every call with one line of error, so
    /// the model reads that and stops asking.
    async fn run_tool(&self, call: &ToolCall) -> ToolResult {
        let meetings = self.app.try_state::<Arc<MeetingSessionManager>>();
        let history = self.app.try_state::<Arc<HistoryManager>>();
        let calendar = self.app.try_state::<Arc<dyn CalendarSource>>();
        match (meetings, history, calendar) {
            (Some(meetings), Some(history), Some(calendar)) => {
                tools::run(meetings.inner(), history.inner(), calendar.inner(), call).await
            }
            _ => ToolResult {
                id: call.id.clone(),
                tool: call.tool.clone(),
                ok: false,
                result: "Sona tools are not available in this process".to_string(),
                sources: Vec::new(),
            },
        }
    }

    pub(crate) async fn cancel_turn(
        &self,
        request: AgentPanelCancelTurnRequestV1,
    ) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
        let should_submit = {
            let mut state = self.lock_state();
            let active = state
                .turn
                .as_mut()
                .filter(|active| active.turn_id == request.turn_id)
                .ok_or(AgentPanelCommandErrorV1::UnknownTurn)?;
            if active.state.is_terminal() {
                return Ok(state.status());
            }
            active.cancel_requested = true;
            active.set_state(AgentPanelTurnStateV1::Canceling);
            let should_submit = active.job_id.is_none() && !active.submitting;
            let invalidation_id = state.invalidate();
            self.emit_turn(
                invalidation_id,
                Some(request.turn_id.clone()),
                Some(AgentPanelTurnStateV1::Canceling),
            );
            should_submit
        };
        if should_submit {
            self.submit_active_turn(&request.turn_id).await?;
        } else {
            self.cancel_known_turn(&request.turn_id).await?;
        }
        Ok(self.current_status())
    }

    async fn cancel_known_turn(&self, turn_id: &str) -> Result<(), AgentPanelCommandErrorV1> {
        let (job_id, workspace) = {
            let state = self.lock_state();
            let active = state
                .turn
                .as_ref()
                .filter(|active| active.turn_id == turn_id)
                .ok_or(AgentPanelCommandErrorV1::UnknownTurn)?;
            let Some(job_id) = active.job_id.clone() else {
                return Ok(());
            };
            if active.state.is_terminal() {
                return Ok(());
            }
            (job_id, active.workspace)
        };
        let result = match RelayClient::from_settings(&self.app, self.nonce_cache.clone()).await {
            Ok(client) => client.cancel_job(&job_id, workspace).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(job) => {
                let follow_up = self.accept_job(turn_id, job)?;
                if !follow_up.terminal {
                    self.start_polling();
                }
                Ok(())
            }
            Err(error) => {
                self.record_relay_error(turn_id, error, false);
                Err(map_relay_error(error))
            }
        }
    }

    fn accept_job(
        &self,
        turn_id: &str,
        job: RelayJob,
    ) -> Result<JobFollowUp, AgentPanelCommandErrorV1> {
        let auto_apply_enabled =
            crate::settings::get_settings(&self.app).agent_panel_safe_appearance_auto_apply;
        let accepted = {
            let mut state = self.lock_state();
            accept_job_in_state(&mut state, turn_id, job, auto_apply_enabled)
        };
        let AcceptedRelayJob {
            invalidation_id,
            turn_state,
            proposal_event,
            follow_up,
        } = match accepted {
            Ok(accepted) => accepted,
            /* Each refusal is recorded beside the return that carries it out,
             * and the match names every one: a refusal added inside the
             * extracted function does not compile until somebody has decided
             * what it tells the reader. */
            Err(RelayJobRefusal::UnknownTurn) => return Err(AgentPanelCommandErrorV1::UnknownTurn),
            Err(RelayJobRefusal::OwnershipRejected) => {
                self.record_relay_error(turn_id, RelayError::OwnershipRejected, false);
                return Err(AgentPanelCommandErrorV1::OwnershipRejected);
            }
            Err(RelayJobRefusal::InvalidProposal) => {
                self.record_protocol_failure(turn_id, AgentPanelRelayStatusV1::UntrustedResponse);
                return Err(AgentPanelCommandErrorV1::InvalidProposal);
            }
            Err(RelayJobRefusal::UntrustedResponse) => {
                self.record_protocol_failure(turn_id, AgentPanelRelayStatusV1::UntrustedResponse);
                return Err(AgentPanelCommandErrorV1::UntrustedResponse);
            }
            Err(RelayJobRefusal::WorkspaceMismatch) => {
                self.record_protocol_failure(turn_id, AgentPanelRelayStatusV1::WorkspaceMismatch);
                return Err(AgentPanelCommandErrorV1::WorkspaceMismatch);
            }
        };
        self.emit_status(invalidation_id, AgentPanelRelayStatusV1::Ready);
        self.remember_conversation();
        self.emit_turn(invalidation_id, Some(turn_id.to_string()), Some(turn_state));
        if let Some((proposal_id, proposal_state)) = proposal_event {
            self.emit_proposal(invalidation_id, Some(proposal_id), Some(proposal_state));
        }
        Ok(follow_up)
    }

    /// Make one offered change, once.
    ///
    /// A card that is not pending is already answered, so this returns what it
    /// says rather than refusing: two presses on Apply — a double click, a
    /// reopened sheet, a retry after a slow round trip — must leave one
    /// mutation and one receipt behind.
    pub(crate) async fn apply_action(
        &self,
        meetings: &MeetingSessionManager,
        request: AgentPanelActionRequestV1,
    ) -> Result<AgentPanelTurnStatusV1, AgentPanelCommandErrorV1> {
        let to_run = {
            let state = self.lock_state();
            self.stored_action(&state, &request)?.to_run().cloned()
        };
        let Some(action) = to_run else {
            return self.turn_status(&request.turn_id);
        };
        let applied = actions::apply(&self.app, meetings, &action)
            .await
            .map_err(|_| AgentPanelCommandErrorV1::ActionFailed)?;
        self.settle_action(&request, StoredActionState::Applied(applied))
    }

    /// Put one offered change back: refuse it before it happens, or reverse it
    /// after.
    ///
    /// One command for both because they are one gesture — "this change is not
    /// in effect" — and because an action that has been undone is in exactly
    /// the state an action that was never applied is in. Reversing runs the
    /// inverse mutation, which earns its own receipt: the corpus was changed
    /// twice and the ledger says so.
    pub(crate) async fn dismiss_action(
        &self,
        meetings: &MeetingSessionManager,
        request: AgentPanelActionRequestV1,
    ) -> Result<AgentPanelTurnStatusV1, AgentPanelCommandErrorV1> {
        let undo = {
            let state = self.lock_state();
            match self.stored_action(&state, &request)?.reversal() {
                Reversal::Settled => return self.turn_status(&request.turn_id),
                Reversal::Unapplied => None,
                Reversal::Undo(undo) => Some(undo.clone()),
            }
        };
        if let Some(undo) = undo {
            actions::undo(&self.app, meetings, &undo)
                .await
                .map_err(|_| AgentPanelCommandErrorV1::ActionFailed)?;
        }
        self.settle_action(&request, StoredActionState::Dismissed)
    }

    fn stored_action<'a>(
        &self,
        state: &'a PanelState,
        request: &AgentPanelActionRequestV1,
    ) -> Result<&'a StoredAction, AgentPanelCommandErrorV1> {
        let index = usize::try_from(request.action_index)
            .map_err(|_| AgentPanelCommandErrorV1::UnknownAction)?;
        state
            .turn
            .as_ref()
            .filter(|active| active.turn_id == request.turn_id)
            .ok_or(AgentPanelCommandErrorV1::UnknownTurn)?
            .actions
            .get(index)
            .ok_or(AgentPanelCommandErrorV1::UnknownAction)
    }

    /// Record what a mutation did to one card, and tell the sheet.
    ///
    /// The turn is re-found rather than held across the write: the mutation
    /// awaits, and holding the panel lock across an await would block every
    /// other command on this surface behind a store round trip.
    fn settle_action(
        &self,
        request: &AgentPanelActionRequestV1,
        settled: StoredActionState,
    ) -> Result<AgentPanelTurnStatusV1, AgentPanelCommandErrorV1> {
        let index = usize::try_from(request.action_index)
            .map_err(|_| AgentPanelCommandErrorV1::UnknownAction)?;
        let (invalidation_id, status) = {
            let mut state = self.lock_state();
            let active = state
                .turn
                .as_mut()
                .filter(|active| active.turn_id == request.turn_id)
                .ok_or(AgentPanelCommandErrorV1::UnknownTurn)?;
            active
                .actions
                .get_mut(index)
                .ok_or(AgentPanelCommandErrorV1::UnknownAction)?
                .state = settled;
            let status = active.status();
            (state.invalidate(), status)
        };
        self.emit_turn(
            invalidation_id,
            Some(status.turn_id.clone()),
            Some(status.state),
        );
        Ok(status)
    }

    fn turn_status(
        &self,
        turn_id: &str,
    ) -> Result<AgentPanelTurnStatusV1, AgentPanelCommandErrorV1> {
        self.lock_state()
            .turn
            .as_ref()
            .filter(|active| active.turn_id == turn_id)
            .map(ActiveTurn::status)
            .ok_or(AgentPanelCommandErrorV1::UnknownTurn)
    }

    pub(crate) fn apply_change(
        &self,
        request: AgentPanelApplyChangeRequestV1,
    ) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
        self.apply_proposal(
            &request.proposal_id,
            Some(request.expected_revision),
            request.confirmed,
        )?;
        Ok(self.current_status())
    }

    /// Apply every change a proposal carries, as one revision.
    ///
    /// A proposal is one offer. The card names the whole change set and
    /// carries one Apply, so applying part of it and then reporting the whole
    /// card applied — which is what a per-action apply did, because the
    /// receipt is the proposal's — would be a card that lied about what it
    /// did. The safe-appearance auto path and the button are therefore the
    /// same operation, differing only in what authorised it.
    ///
    /// `expected_revision` is the caller's claim about which settings the
    /// proposal was written against; the auto path has no claim of its own to
    /// make because it runs on the proposal the moment it arrives.
    fn apply_proposal(
        &self,
        proposal_id: &str,
        expected_revision: Option<u64>,
        confirmed: bool,
    ) -> Result<(), AgentPanelCommandErrorV1> {
        let (source_revision, allowed, changes) = {
            let state = self.lock_state();
            let proposal = state
                .proposal
                .as_ref()
                .filter(|proposal| proposal.id == proposal_id)
                .ok_or(AgentPanelCommandErrorV1::UnknownProposal)?;
            if proposal.state != AgentPanelProposalStateV1::Pending {
                return Err(AgentPanelCommandErrorV1::NotUndoable);
            }
            if expected_revision
                .is_some_and(|revision| proposal.proposal.source_settings_revision != revision)
            {
                return Err(AgentPanelCommandErrorV1::StaleProposal);
            }
            if proposal.proposal.actions.is_empty() {
                return Err(AgentPanelCommandErrorV1::InvalidProposal);
            }
            if strongest_confirmation(&proposal.proposal.actions)
                != SonaConfirmationClassV1::Automatic
                && !confirmed
            {
                return Err(AgentPanelCommandErrorV1::ConfirmationRequired);
            }
            (
                proposal.proposal.source_settings_revision,
                proposal.allowed.clone(),
                proposal.proposal.actions.clone(),
            )
        };
        let applied = match config::apply_changes(&self.app, source_revision, &changes, &allowed) {
            Ok(applied) => applied,
            Err(error) => {
                self.record_config_error(proposal_id, error);
                return Err(map_config_error(error));
            }
        };
        self.store_applied_receipt(proposal_id, applied)
    }

    /// The one proposal shape that applies itself: every action in it is an
    /// appearance change the reader can see and reverse at a glance, and the
    /// setting that allows it is off on install.
    ///
    /// It claims no confirmation of its own. An all-`Automatic` set needs
    /// none, so the gate in [`Self::apply_proposal`] passes on the actions'
    /// own class rather than on this path's say-so — which is what keeps a
    /// future non-automatic change from riding in on the auto flag.
    fn apply_safe_appearance_proposal(
        &self,
        proposal_id: &str,
    ) -> Result<(), AgentPanelCommandErrorV1> {
        {
            let state = self.lock_state();
            let Some(proposal) = state
                .proposal
                .as_ref()
                .filter(|proposal| proposal.id == proposal_id)
            else {
                return Err(AgentPanelCommandErrorV1::UnknownProposal);
            };
            if proposal.state != AgentPanelProposalStateV1::Pending
                || proposal.proposal.actions.is_empty()
                || !proposal
                    .proposal
                    .actions
                    .iter()
                    .all(SonaSettingChangeV1::is_auto_eligible)
            {
                return Ok(());
            }
        }
        self.apply_proposal(proposal_id, None, false)
    }

    fn store_applied_receipt(
        &self,
        proposal_id: &str,
        applied: AppliedSettings,
    ) -> Result<(), AgentPanelCommandErrorV1> {
        let (invalidation_id, proposal_state) = {
            let mut state = self.lock_state();
            let proposal_state = {
                let proposal = state
                    .proposal
                    .as_mut()
                    .filter(|proposal| proposal.id == proposal_id)
                    .ok_or(AgentPanelCommandErrorV1::UnknownProposal)?;
                if proposal.state != AgentPanelProposalStateV1::Pending {
                    return Err(AgentPanelCommandErrorV1::NotUndoable);
                }
                proposal.state = AgentPanelProposalStateV1::Applied;
                proposal.receipt = Some(AppliedReceipt {
                    id: format!("receipt-{proposal_id}-{}", applied.revision),
                    revision: applied.revision,
                    undo: applied.undo().to_vec(),
                });
                proposal.state
            };
            let invalidation_id = state.invalidate();
            (invalidation_id, proposal_state)
        };
        self.emit_proposal(
            invalidation_id,
            Some(proposal_id.to_string()),
            Some(proposal_state),
        );
        Ok(())
    }

    pub(crate) fn undo_change(
        &self,
        request: AgentPanelUndoChangeRequestV1,
    ) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
        let (proposal_id, undo) = {
            let state = self.lock_state();
            let proposal = state
                .proposal
                .as_ref()
                .ok_or(AgentPanelCommandErrorV1::NotUndoable)?;
            let receipt = proposal
                .receipt
                .as_ref()
                .filter(|receipt| receipt.id == request.receipt_id)
                .ok_or(AgentPanelCommandErrorV1::NotUndoable)?;
            if proposal.state != AgentPanelProposalStateV1::Applied
                || receipt.revision != request.expected_revision
            {
                return Err(AgentPanelCommandErrorV1::StaleProposal);
            }
            (proposal.id.clone(), receipt.undo.clone())
        };
        let _revision = match config::undo_changes(&self.app, request.expected_revision, &undo) {
            Ok(revision) => revision,
            Err(error) => {
                self.record_config_error(&proposal_id, error);
                return Err(map_config_error(error));
            }
        };
        let (invalidation_id, proposal_state) = {
            let mut state = self.lock_state();
            let proposal_state = {
                let proposal = state
                    .proposal
                    .as_mut()
                    .filter(|proposal| proposal.id == proposal_id)
                    .ok_or(AgentPanelCommandErrorV1::NotUndoable)?;
                let receipt = proposal
                    .receipt
                    .as_ref()
                    .ok_or(AgentPanelCommandErrorV1::NotUndoable)?;
                if receipt.revision != request.expected_revision {
                    return Err(AgentPanelCommandErrorV1::StaleProposal);
                }
                proposal.receipt = None;
                proposal.state = AgentPanelProposalStateV1::Undone;
                proposal.state
            };
            let invalidation_id = state.invalidate();
            (invalidation_id, proposal_state)
        };
        self.emit_proposal(invalidation_id, Some(proposal_id), Some(proposal_state));
        Ok(self.current_status())
    }

    fn record_config_error(&self, proposal_id: &str, error: ConfigError) {
        let (invalidation_id, state) = {
            let mut panel = self.lock_state();
            if let Some(proposal) = panel
                .proposal
                .as_mut()
                .filter(|proposal| proposal.id == proposal_id)
            {
                proposal.state = AgentPanelProposalStateV1::Rejected;
                proposal.receipt = None;
            }
            if matches!(error, ConfigError::StaleRevision) {
                panel.relay_status = AgentPanelRelayStatusV1::Ready;
            }
            let invalidation_id = panel.invalidate();
            (
                invalidation_id,
                panel.proposal.as_ref().map(|proposal| proposal.state),
            )
        };
        self.emit_proposal(invalidation_id, Some(proposal_id.to_string()), state);
    }

    fn record_relay_error(&self, turn_id: &str, error: RelayError, submit_failure: bool) {
        let relay_status = relay_status_for_error(error);
        let retryable = matches!(error, RelayError::RequestFailed) && !submit_failure;
        let (invalidation_id, turn_state) = {
            let mut state = self.lock_state();
            state.relay_status = relay_status;
            let turn_state = state
                .turn
                .as_mut()
                .filter(|active| active.turn_id == turn_id)
                .map(|active| {
                    active.submitting = false;
                    if !retryable {
                        active.fail(turn_failure_for_relay_error(error));
                    }
                    active.state
                });
            let invalidation_id = state.invalidate();
            (invalidation_id, turn_state)
        };
        self.emit_status(invalidation_id, relay_status);
        self.emit_turn(invalidation_id, Some(turn_id.to_string()), turn_state);
    }

    fn record_protocol_failure(&self, turn_id: &str, relay_status: AgentPanelRelayStatusV1) {
        let (invalidation_id, turn_state) = {
            let mut state = self.lock_state();
            state.relay_status = relay_status;
            let turn_state = state
                .turn
                .as_mut()
                .filter(|active| active.turn_id == turn_id)
                .map(|active| {
                    active.fail(AgentPanelTurnFailureV1::Failed);
                    active.state
                });
            let invalidation_id = state.invalidate();
            (invalidation_id, turn_state)
        };
        self.emit_status(invalidation_id, relay_status);
        self.emit_turn(invalidation_id, Some(turn_id.to_string()), turn_state);
    }

    fn start_polling(&self) {
        let generation = self
            .poll_generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        if self.poll_plan(generation).is_none() {
            return;
        }
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            poll_loop(app, generation).await;
        });
    }

    /// Stop the poll loop.
    ///
    /// `pub(crate)` because switching the agent off in Settings has to reach
    /// it: the relay is no longer allowed to be talked to, and a loop left
    /// running would keep talking to it until the turn finished.
    pub(crate) fn stop_polling(&self) {
        self.poll_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn poll_plan(&self, generation: u64) -> Option<PollPlan> {
        if self.poll_generation.load(Ordering::Acquire) != generation {
            return None;
        }
        /* Polling follows the turn, not the surface. The sheet closing is a
         * fold in a layout; the job on the far side is still running, and its
         * answer belongs in the scrollback whether or not anyone is looking. */
        let state = self.lock_state();
        let active = state.turn.as_ref()?;
        let job_id = active.job_id.clone()?;
        if active.state.is_terminal() {
            return None;
        }
        let delay = if active.last_progress.elapsed() >= IDLE_POLL_AFTER {
            IDLE_POLL_INTERVAL
        } else {
            POLL_INTERVAL
        };
        Some(PollPlan {
            turn_id: active.turn_id.clone(),
            workspace: active.workspace,
            job_id,
            event_cursor: active.event_cursor,
            delay,
        })
    }

    fn accept_events(
        &self,
        turn_id: &str,
        job_id: &str,
        events: Vec<RelayEvent>,
    ) -> Result<(), AgentPanelCommandErrorV1> {
        if events.is_empty() {
            return Ok(());
        }
        let (invalidation_id, turn_state) = {
            let mut state = self.lock_state();
            let turn_state = {
                let active = state
                    .turn
                    .as_mut()
                    .filter(|active| {
                        active.turn_id == turn_id && active.job_id.as_deref() == Some(job_id)
                    })
                    .ok_or(AgentPanelCommandErrorV1::OwnershipRejected)?;
                for event in events {
                    if event.id <= active.event_cursor || event.event_type.is_empty() {
                        return Err(AgentPanelCommandErrorV1::UntrustedResponse);
                    }
                    active.event_cursor = event.id;
                    active.last_progress = Instant::now();
                }
                active.state
            };
            let invalidation_id = state.invalidate();
            (invalidation_id, turn_state)
        };
        self.emit_turn(invalidation_id, Some(turn_id.to_string()), Some(turn_state));
        Ok(())
    }

    pub(crate) async fn shutdown(&self) {
        self.stop_polling();
        let pending = {
            let mut state = self.lock_state();
            state.turn.as_mut().and_then(|active| {
                if active.state.is_terminal() {
                    None
                } else {
                    active.cancel_requested = true;
                    active
                        .job_id
                        .clone()
                        .map(|job_id| (job_id, active.workspace))
                }
            })
        };
        let Some((job_id, workspace)) = pending else {
            return;
        };
        if let Ok(client) = RelayClient::from_settings(&self.app, self.nonce_cache.clone()).await {
            let _ = client.cancel_job(&job_id, workspace).await;
        }
    }

    fn emit_status(&self, invalidation_id: u64, status: AgentPanelRelayStatusV1) {
        let _ = self.app.emit(
            AgentPanelStatusChangedEvent::NAME,
            AgentPanelStatusChangedEvent {
                invalidation_id,
                status,
            },
        );
    }

    fn emit_turn(
        &self,
        invalidation_id: u64,
        turn_id: Option<String>,
        state: Option<AgentPanelTurnStateV1>,
    ) {
        let _ = self.app.emit(
            AgentPanelTurnChangedEvent::NAME,
            AgentPanelTurnChangedEvent {
                invalidation_id,
                turn_id,
                state,
            },
        );
    }

    fn emit_proposal(
        &self,
        invalidation_id: u64,
        proposal_id: Option<String>,
        state: Option<AgentPanelProposalStateV1>,
    ) {
        let _ = self.app.emit(
            AgentPanelProposalChangedEvent::NAME,
            AgentPanelProposalChangedEvent {
                invalidation_id,
                proposal_id,
                state,
            },
        );
    }
}

struct PollPlan {
    turn_id: String,
    workspace: AgentPanelWorkspaceV1,
    job_id: String,
    event_cursor: u64,
    delay: Duration,
}

struct JobFollowUp {
    proposal_id: Option<String>,
    auto_apply: bool,
    cancel_requested: bool,
    terminal: bool,
    /// The job finished with lookups to run rather than an answer.
    tool_calls: bool,
}

struct AcceptedRelayJob {
    invalidation_id: u64,
    turn_state: AgentPanelTurnStateV1,
    proposal_event: Option<(String, AgentPanelProposalStateV1)>,
    follow_up: JobFollowUp,
}

/// Why one relay answer was not taken.
///
/// Its own enum rather than [`AgentPanelCommandErrorV1`], because each of
/// these owes the reader something before the command returns — a relay error
/// for one, the protocol failure behind
/// [`AgentPanelRelayStatusV1::UntrustedResponse`] for two — and the caller
/// matches every variant, so a refusal added here stops compiling until its
/// recording is decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RelayJobRefusal {
    /// The turn this answer belongs to is gone: no counter to move, and
    /// nothing on screen to correct.
    UnknownTurn,
    OwnershipRejected,
    InvalidProposal,
    UntrustedResponse,
    /// The answer was signed, and answers the other workspace: a proposal on
    /// an Ask turn, or prose on a Configure one. Held apart from
    /// [`RelayJobRefusal::UntrustedResponse`] because the reader's fix differs
    /// — this one is the relay's own bug, not a key that stopped matching.
    WorkspaceMismatch,
}

/// Which refusal a rejected answer is.
///
/// The error decides, not the shape that carried it: a mismatch is a
/// well-formed answer on the workspace that did not ask, and either shape can
/// be one — a proposal on an Ask turn, prose on a Configure one.
fn refusal_for_validation(
    error: ProposalValidationError,
    response: &SonaAgentResponseV1,
) -> RelayJobRefusal {
    match error {
        ProposalValidationError::WorkspaceMismatch => RelayJobRefusal::WorkspaceMismatch,
        _ if matches!(response, SonaAgentResponseV1::Proposal { .. }) => {
            RelayJobRefusal::InvalidProposal
        }
        _ => RelayJobRefusal::UntrustedResponse,
    }
}

fn accept_job_in_state(
    state: &mut PanelState,
    turn_id: &str,
    job: RelayJob,
    auto_apply_enabled: bool,
) -> Result<AcceptedRelayJob, RelayJobRefusal> {
    let RelayJob {
        id: job_id,
        state: relay_state,
        response,
        failure,
    } = job;
    /* One visit to the turn: checking whose answer this is and writing what it
     * said are the same lookup, and nothing can move the state between them. */
    let (cancel_requested, turn_state, allowed, tool_calls) = {
        let active = state
            .turn
            .as_mut()
            .filter(|active| active.turn_id == turn_id)
            .ok_or(RelayJobRefusal::UnknownTurn)?;
        if active
            .job_id
            .as_deref()
            .is_some_and(|existing| existing != job_id)
        {
            return Err(RelayJobRefusal::OwnershipRejected);
        }
        if let Some(response) = response.as_ref() {
            if let Err(error) = response.validate(&active.request, &active.allowed) {
                return Err(refusal_for_validation(error, response));
            }
        }
        active.last_progress = Instant::now();
        let lookups = match response.as_ref() {
            Some(SonaAgentResponseV1::ToolCalls { calls, .. })
                if relay_state == RelayJobStateV1::Succeeded =>
            {
                Some(calls)
            }
            _ => None,
        };
        match lookups {
            Some(calls) => {
                active.job_id = None;
                active.pending_calls.clone_from(calls);
                if active.cancel_requested {
                    active.submitting = false;
                    active.set_state(AgentPanelTurnStateV1::Canceled);
                } else {
                    active.submitting = true;
                    active.set_state(AgentPanelTurnStateV1::Running);
                }
            }
            None => {
                active.job_id = Some(job_id);
                active.submitting = false;
                active.set_state(turn_state_for_job(&relay_state, active.cancel_requested));
            }
        }
        if let Some(failure) = failure {
            active.failure.get_or_insert(turn_failure_for_job(failure));
        }
        if let Some(response) = response.as_ref() {
            let elapsed_ms = active.elapsed_ms();
            merge_steps(&mut active.steps, response.steps(), elapsed_ms);
        }
        if let Some(SonaAgentResponseV1::Text { actions, .. }) = response.as_ref() {
            active.actions = actions.iter().cloned().map(StoredAction::pending).collect();
        }
        (
            active.cancel_requested,
            active.state,
            active.allowed.clone(),
            lookups.is_some(),
        )
    };
    state.relay_status = AgentPanelRelayStatusV1::Ready;

    let mut proposal_event = None;
    let mut auto_apply = false;
    match response {
        Some(SonaAgentResponseV1::Text { message, .. }) => {
            state.push_conversation(SonaAgentChatTurnV1 {
                role: SonaAgentChatRoleV1::Assistant,
                message,
            });
        }
        Some(SonaAgentResponseV1::Proposal { proposal, .. }) => {
            let proposal_id = format!("proposal-{turn_id}");
            let summary = proposal.summary.clone();
            let all_safe_appearance = !proposal.actions.is_empty()
                && proposal
                    .actions
                    .iter()
                    .all(SonaSettingChangeV1::is_auto_eligible);
            state.push_conversation(SonaAgentChatTurnV1 {
                role: SonaAgentChatRoleV1::Assistant,
                message: summary,
            });
            state.proposal = Some(StoredProposal {
                id: proposal_id.clone(),
                proposal,
                allowed,
                state: AgentPanelProposalStateV1::Pending,
                receipt: None,
            });
            proposal_event = Some((proposal_id, AgentPanelProposalStateV1::Pending));
            auto_apply = auto_apply_enabled && all_safe_appearance;
        }
        Some(SonaAgentResponseV1::ToolCalls { .. }) | None => {}
    }
    let invalidation_id = state.invalidate();
    Ok(AcceptedRelayJob {
        invalidation_id,
        turn_state,
        proposal_event: proposal_event.clone(),
        follow_up: JobFollowUp {
            proposal_id: proposal_event.map(|event| event.0),
            auto_apply,
            cancel_requested,
            terminal: turn_state.is_terminal(),
            tool_calls,
        },
    })
}
/// The step id of one lookup: the round and the call's position in it. Not
/// the model's call id, which the relay does not hold unique, so two calls
/// that share one are still two rows.
fn tool_step_id(round: usize, index: usize) -> String {
    format!("tool-{round}-{index}")
}

/// The pack with one round's results appended, inside the panel's ceiling.
///
/// The block is what the relay's prompt describes: a round header, then one
/// `call` line per lookup followed by its result as JSON or its one-line
/// error. When the whole block does not fit under [`MAX_CONTEXT_PACK_BYTES`],
/// results are cut from the last call backwards, each replaced by a line
/// saying so, and the header says the block was cut; a model that answered
/// from a silently shortened block would say "the lookup returned nothing".
/// A pack too full for even the header is returned unchanged.
fn append_tool_block(
    pack: &str,
    round: usize,
    calls: &[ToolCall],
    results: &[ToolResult],
) -> String {
    let render = |kept: usize| {
        let mut block = format!(
            "tool results round {round} of {MAX_TOOL_ROUNDS}{}",
            if kept < results.len() { " (cut)" } else { "" }
        );
        for (index, (call, result)) in calls.iter().zip(results).enumerate() {
            // Compact JSON escapes every control byte, so the args stay on
            // their line whatever the model put in a string.
            let args = serde_json::to_string(&call.args).unwrap_or_else(|_| "{}".to_string());
            block.push_str(&format!(
                "\ncall {} {} {args} {}\n{}",
                result.id,
                result.tool,
                if result.ok { "ok" } else { "error" },
                if index < kept {
                    result.result.as_str()
                } else {
                    "result cut at the pack ceiling"
                }
            ));
        }
        block
    };
    let separator = usize::from(!pack.is_empty()) * 2;
    for kept in (0..=results.len()).rev() {
        let block = render(kept);
        if pack.len() + separator + block.len() <= MAX_CONTEXT_PACK_BYTES {
            return if pack.is_empty() {
                block
            } else {
                format!("{pack}\n\n{block}")
            };
        }
    }
    pack.to_string()
}

async fn poll_loop(app: AppHandle, generation: u64) {
    loop {
        let plan = {
            let manager = app.state::<AgentPanelManager>();
            manager.poll_plan(generation)
        };
        let Some(plan) = plan else {
            return;
        };
        if let Err(error) = poll_once(&app, &plan).await {
            /* The refusal was already named where it was raised; the loop
             * records the status again, so it must not flatten a mismatch back
             * into an answer this side could not verify. */
            let manager = app.state::<AgentPanelManager>();
            manager.record_protocol_failure(&plan.turn_id, protocol_failure_status(error));
            return;
        }
        tokio::time::sleep(plan.delay).await;
    }
}

async fn poll_once(app: &AppHandle, plan: &PollPlan) -> Result<(), AgentPanelCommandErrorV1> {
    let (nonce_cache, app_handle) = {
        let manager = app.state::<AgentPanelManager>();
        (manager.nonce_cache.clone(), manager.app.clone())
    };
    let client = match RelayClient::from_settings(&app_handle, nonce_cache).await {
        Ok(client) => client,
        Err(error) => {
            let manager = app.state::<AgentPanelManager>();
            manager.record_relay_error(&plan.turn_id, error, false);
            return if matches!(error, RelayError::RequestFailed) {
                Ok(())
            } else {
                Err(map_relay_error(error))
            };
        }
    };
    let job = match client.get_job(&plan.job_id, plan.workspace).await {
        Ok(job) => job,
        Err(error) => {
            let manager = app.state::<AgentPanelManager>();
            manager.record_relay_error(&plan.turn_id, error, false);
            return if matches!(error, RelayError::RequestFailed) {
                Ok(())
            } else {
                Err(map_relay_error(error))
            };
        }
    };
    let follow_up = {
        let manager = app.state::<AgentPanelManager>();
        manager.accept_job(&plan.turn_id, job)?
    };
    if follow_up.auto_apply {
        if let Some(proposal_id) = follow_up.proposal_id.as_deref() {
            let manager = app.state::<AgentPanelManager>();
            manager.apply_safe_appearance_proposal(proposal_id)?;
        }
    }
    if follow_up.cancel_requested {
        let manager = app.state::<AgentPanelManager>();
        manager.cancel_known_turn(&plan.turn_id).await?;
        return Ok(());
    }
    if follow_up.tool_calls {
        let manager = app.state::<AgentPanelManager>();
        if manager.run_tool_round(&plan.turn_id).await? {
            /* A resubmission that fails records its reason on the turn before
             * returning, as the first submission did; handed to the loop, the
             * error would stamp the turn a second time, as a protocol
             * failure, over a relay that was merely unreachable. */
            let _ = manager.submit_active_turn(&plan.turn_id).await;
        }
        return Ok(());
    }
    let events = match client.get_events(&plan.job_id, plan.event_cursor).await {
        Ok(events) => events,
        Err(error) => {
            let manager = app.state::<AgentPanelManager>();
            manager.record_relay_error(&plan.turn_id, error, false);
            return if matches!(error, RelayError::RequestFailed) {
                Ok(())
            } else {
                Err(map_relay_error(error))
            };
        }
    };
    let manager = app.state::<AgentPanelManager>();
    manager.accept_events(&plan.turn_id, &plan.job_id, events)
}

/// What the stored pairing says about whether a turn can be sent at all.
///
/// A free function because the panel manager is not the only caller any more:
/// D14's meeting engine and the settings surface that offers it both need the
/// same answer, and a second reading of the same four settings fields is how
/// two surfaces come to disagree about whether a relay exists.
fn configured_relay_status(app: &AppHandle) -> AgentPanelRelayStatusV1 {
    let settings = crate::settings::get_settings(app);
    if !settings.agent_panel_enabled {
        AgentPanelRelayStatusV1::Disabled
    } else if !settings.agent_panel_paired {
        AgentPanelRelayStatusV1::Unpaired
    } else if settings.agent_panel_relay_url.is_none()
        || settings.agent_panel_relay_key_id.is_none()
        || settings.agent_panel_relay_public_key.is_none()
    {
        AgentPanelRelayStatusV1::InvalidConfiguration
    } else {
        AgentPanelRelayStatusV1::Ready
    }
}

/// True when a `sona-chat` turn has somewhere to go: the panel is on, a relay
/// is paired, and the pinned key and its URL are both stored.
pub(crate) fn relay_is_reachable(app: &AppHandle) -> bool {
    configured_relay_status(app) == AgentPanelRelayStatusV1::Ready
}

/// Why a headless turn produced no text, in the only two shapes a caller
/// outside the panel can act on.
///
/// The twelve [`RelayError`] variants are relay semantics and stay in this
/// module: nothing in a meeting can act differently on `OwnershipRejected`
/// than on `ResponseTooLarge`. What it can do is tell its reader "your server
/// was not reached" rather than "these notes could not be written", so that is
/// the one distinction that crosses the boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChatTurnError {
    /// The relay was never reached: switched off, unpaired, misconfigured, or
    /// the network was not there. Nothing ran on the far side.
    Unreachable,
    /// The turn reached the relay and did not come back with usable text.
    Failed,
    /// The turn asked for a structured reply and the relay refused a prose
    /// one.
    ///
    /// Kept apart from `Failed` because it is the only failure here whose
    /// cause a reader can act on: the model answered, in the wrong shape, and
    /// no amount of waiting or reconnecting changes that. It reaches a meeting
    /// as an ordinary failed generation — the meeting's action is the same —
    /// but it is named in the log, which is the part that was missing. This
    /// failure used to arrive as `SUCCEEDED` carrying prose, and cost two days
    /// of diagnosis because no surface on either host said what had happened.
    ReplyNotStructured,
}

/// Which of the two a relay error is.
///
/// Everything meaning "this client cannot talk to a relay at all right now" is
/// `Unreachable`; everything meaning "it talked, and the answer was not one"
/// is `Failed`. `RandomUnavailable` sits with the first group because a client
/// that cannot mint a nonce never sends anything, and `Unauthorized` sits there
/// because a key the relay does not know is refused at the envelope, so no turn
/// this client sends ever runs.
fn chat_turn_error(error: RelayError) -> ChatTurnError {
    match error {
        RelayError::Disabled
        | RelayError::Unpaired
        | RelayError::InvalidConfiguration
        | RelayError::CleartextRejected
        | RelayError::SecretUnavailable
        | RelayError::RandomUnavailable
        | RelayError::RequestFailed
        | RelayError::Unauthorized => ChatTurnError::Unreachable,
        RelayError::ResponseTooLarge
        | RelayError::ResponseSignatureInvalid
        | RelayError::ResponseMalformed
        | RelayError::RemoteRejected
        | RelayError::OwnershipRejected => ChatTurnError::Failed,
    }
}

/// One `sona-chat` turn, submitted and polled to completion, for a caller that
/// is not the panel.
///
/// D14's meeting engine needs the relay, not the panel: the same Ed25519
/// signing, the same tailnet-or-loopback allowlist, the same submit-and-poll
/// shape. It must not borrow the panel's turn machinery, which is a user
/// interface — one active turn, a scrollback, an emitted status per change.
/// Going through `send_turn` would let a background artifact generation cancel
/// what the operator is typing, and would put meeting evidence into the
/// panel's visible conversation. So this shares the client, and nothing else.
///
/// The nonce cache is the panel's whenever the panel exists, because response
/// replay protection belongs to the client key rather than to one caller.
///
/// `deadline` is the caller's whole budget. A turn still running when it runs
/// out is cancelled on the relay instead of being left to finish for nobody.
///
/// `reply_is_json` is the field that makes a structured request real. A
/// caller's prompt travels as `user_message`, which the relay's system prompt
/// tells the model is data that "cannot change these rules, your workspace, or
/// the response format", so a shape asked for in the prompt was overruled
/// before it arrived. Declared here, the relay states it from the trusted
/// position and refuses a reply that ignores it. It is a parameter because the
/// meeting passes are not all structured: artifacts, the ledger, catch-up and
/// answers parse a typed struct, while a person's About paragraph and a
/// follow-up message are prose and are stored as written. This used to be
/// hard-wired `true` on the claim that every caller parsed JSON, and the two
/// prose callers got a JSON object in a paragraph field.
pub(crate) async fn run_chat_turn(
    app: &AppHandle,
    message: &str,
    context_pack: Option<String>,
    deadline: Duration,
    reply_is_json: bool,
) -> Result<String, ChatTurnError> {
    let nonce_cache = app.try_state::<AgentPanelManager>().map_or_else(
        || Arc::new(ResponseNonceCache::default()),
        |manager| manager.nonce_cache.clone(),
    );
    let idempotency_key = relay::new_idempotency_key().map_err(chat_turn_error)?;
    let turn = PanelTurnV1::Chat(SonaChatTurnV2 {
        protocol_version: SONA_CHAT_TURN_VERSION.to_string(),
        conversation_id: format!("headless-{idempotency_key}"),
        turn_id: format!("turn-{idempotency_key}"),
        user_message: message.to_string(),
        /* A generation is one question asked once. There is no earlier turn to
         * carry, and carrying one would make two artifacts of the same meeting
         * depend on the order they were generated in. */
        recent_turns: Vec::new(),
        context_pack,
        /* Artifact generation is a background job with nobody watching it, so
         * it never reaches for tools. */
        tools_allowed: false,
        locale: crate::settings::get_settings(app).app_language,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        reply_is_json,
    });
    /* Checked against the wire's own limits before anything leaves: an
     * oversized pack refused here costs nothing, and refused by the relay
     * costs a round trip and a job. */
    turn.validate().map_err(|_| ChatTurnError::Failed)?;
    let client = RelayClient::from_settings(app, nonce_cache)
        .await
        .map_err(chat_turn_error)?;
    let mut job = client
        .submit_turn(&idempotency_key, &turn)
        .await
        .map_err(chat_turn_error)?;
    let started = Instant::now();
    while !job.state.is_terminal() {
        if started.elapsed() >= deadline {
            let _ = client
                .cancel_job(&job.id, AgentPanelWorkspaceV1::SonaChat)
                .await;
            return Err(ChatTurnError::Unreachable);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        job = client
            .get_job(&job.id, AgentPanelWorkspaceV1::SonaChat)
            .await
            .map_err(chat_turn_error)?;
    }
    if job.state != RelayJobStateV1::Succeeded {
        /* The one failure worth naming here. A meeting does the same thing
         * with it as with any other failed turn, so it does not need a third
         * outcome — but a reader chasing a meeting that reached review holding
         * nothing needs this line, and its absence is what made the same fact
         * take two days to establish. */
        if job.failure == Some(RelayJobFailure::ReplyNotStructured) {
            log::warn!(
                "Relay job {} refused a prose reply to a structured request: the model \
                 answered, in the wrong shape, and no artifact could be parsed from it",
                job.id
            );
            return Err(ChatTurnError::ReplyNotStructured);
        }
        return Err(ChatTurnError::Failed);
    }
    let response = job.response.ok_or(ChatTurnError::Failed)?;
    response
        .validate(&turn, &SonaAllowedValuesV1::default())
        .map_err(|_| ChatTurnError::Failed)?;
    match response {
        SonaAgentResponseV1::Text { message, .. } => Ok(message),
        /* Unreachable through `validate`, which refuses a proposal from the
         * chat workspace and lookups on a turn that sent no grant. Written
         * out rather than unwrapped. */
        SonaAgentResponseV1::Proposal { .. } | SonaAgentResponseV1::ToolCalls { .. } => {
            Err(ChatTurnError::Failed)
        }
    }
}

fn turn_failure_for_job(failure: RelayJobFailure) -> AgentPanelTurnFailureV1 {
    match failure {
        /* A reader is told the same thing either way: the assistant would not
         * answer this turn. The shape of what it did return is a fact for the
         * log and for the meeting engine, not for the panel's scrollback, so
         * the wire enum the frontend renders stays as it was. */
        RelayJobFailure::Refused | RelayJobFailure::ReplyNotStructured => {
            AgentPanelTurnFailureV1::Refused
        }
        RelayJobFailure::Failed => AgentPanelTurnFailureV1::Failed,
    }
}

fn turn_failure_for_relay_error(error: RelayError) -> AgentPanelTurnFailureV1 {
    match error {
        RelayError::Disabled
        | RelayError::Unpaired
        | RelayError::InvalidConfiguration
        | RelayError::CleartextRejected
        | RelayError::SecretUnavailable
        | RelayError::RandomUnavailable
        | RelayError::RequestFailed
        | RelayError::Unauthorized => AgentPanelTurnFailureV1::Unreachable,
        RelayError::RemoteRejected => AgentPanelTurnFailureV1::Refused,
        RelayError::ResponseTooLarge
        | RelayError::ResponseSignatureInvalid
        | RelayError::ResponseMalformed
        | RelayError::OwnershipRejected => AgentPanelTurnFailureV1::Failed,
    }
}

fn turn_state_for_job(state: &RelayJobStateV1, cancel_requested: bool) -> AgentPanelTurnStateV1 {
    if cancel_requested && !state.is_terminal() {
        return AgentPanelTurnStateV1::Canceling;
    }
    match state {
        RelayJobStateV1::Queued => AgentPanelTurnStateV1::Queued,
        RelayJobStateV1::Leased => AgentPanelTurnStateV1::Leased,
        RelayJobStateV1::Running => AgentPanelTurnStateV1::Running,
        RelayJobStateV1::WaitingUser => AgentPanelTurnStateV1::WaitingUser,
        RelayJobStateV1::WaitingApproval => AgentPanelTurnStateV1::WaitingApproval,
        RelayJobStateV1::Succeeded => AgentPanelTurnStateV1::Succeeded,
        RelayJobStateV1::Failed => AgentPanelTurnStateV1::Failed,
        RelayJobStateV1::Canceled => AgentPanelTurnStateV1::Canceled,
        RelayJobStateV1::UnverifiedExternal => AgentPanelTurnStateV1::UnverifiedExternal,
    }
}

fn strongest_confirmation(changes: &[SonaSettingChangeV1]) -> SonaConfirmationClassV1 {
    if changes
        .iter()
        .any(|change| change.confirmation_class() == SonaConfirmationClassV1::Explicit)
    {
        SonaConfirmationClassV1::Explicit
    } else if changes
        .iter()
        .any(|change| change.confirmation_class() == SonaConfirmationClassV1::Review)
    {
        SonaConfirmationClassV1::Review
    } else {
        SonaConfirmationClassV1::Automatic
    }
}

fn is_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn relay_status_for_error(error: RelayError) -> AgentPanelRelayStatusV1 {
    match error {
        RelayError::Disabled => AgentPanelRelayStatusV1::Disabled,
        RelayError::Unpaired => AgentPanelRelayStatusV1::Unpaired,
        RelayError::InvalidConfiguration | RelayError::CleartextRejected => {
            AgentPanelRelayStatusV1::InvalidConfiguration
        }
        RelayError::SecretUnavailable => AgentPanelRelayStatusV1::SecretUnavailable,
        RelayError::RequestFailed => AgentPanelRelayStatusV1::Offline,
        RelayError::ResponseSignatureInvalid
        | RelayError::ResponseMalformed
        | RelayError::ResponseTooLarge => AgentPanelRelayStatusV1::UntrustedResponse,
        RelayError::RemoteRejected | RelayError::RandomUnavailable | RelayError::Unauthorized => {
            AgentPanelRelayStatusV1::RemoteRejected
        }
        RelayError::OwnershipRejected => AgentPanelRelayStatusV1::OwnershipRejected,
    }
}

/// The status a poll that ended in a refusal leaves on the pairing row.
///
/// Every refusal but one says the far side spoke something this Mac would not
/// take, which is the same row a bad signature lands on. The mismatch is the
/// exception: it verified, and answered the workspace that did not ask.
const fn protocol_failure_status(error: AgentPanelCommandErrorV1) -> AgentPanelRelayStatusV1 {
    match error {
        AgentPanelCommandErrorV1::WorkspaceMismatch => AgentPanelRelayStatusV1::WorkspaceMismatch,
        _ => AgentPanelRelayStatusV1::UntrustedResponse,
    }
}

fn map_relay_error(error: RelayError) -> AgentPanelCommandErrorV1 {
    match error {
        RelayError::Disabled => AgentPanelCommandErrorV1::Disabled,
        RelayError::Unpaired => AgentPanelCommandErrorV1::Unpaired,
        RelayError::InvalidConfiguration | RelayError::CleartextRejected => {
            AgentPanelCommandErrorV1::InvalidConfiguration
        }
        RelayError::SecretUnavailable => AgentPanelCommandErrorV1::SecretUnavailable,
        RelayError::RequestFailed => AgentPanelCommandErrorV1::Offline,
        RelayError::ResponseSignatureInvalid
        | RelayError::ResponseMalformed
        | RelayError::ResponseTooLarge => AgentPanelCommandErrorV1::UntrustedResponse,
        RelayError::RemoteRejected | RelayError::RandomUnavailable | RelayError::Unauthorized => {
            AgentPanelCommandErrorV1::RemoteRejected
        }
        RelayError::OwnershipRejected => AgentPanelCommandErrorV1::OwnershipRejected,
    }
}

fn map_config_error(error: ConfigError) -> AgentPanelCommandErrorV1 {
    match error {
        ConfigError::SnapshotUnavailable => AgentPanelCommandErrorV1::Offline,
        ConfigError::StaleRevision => AgentPanelCommandErrorV1::StaleProposal,
        ConfigError::InvalidProposal => AgentPanelCommandErrorV1::InvalidProposal,
        ConfigError::InvalidSetting => AgentPanelCommandErrorV1::InvalidSetting,
        ConfigError::NotPersisted => AgentPanelCommandErrorV1::ActionFailed,
    }
}

/// Whether a webview label may reach this surface.
///
/// One label, because there is one window. Every command below used to be
/// split between the main window and the companion panel's own webview; the
/// panel is a sheet inside the main window now, so the panel label is not a
/// narrower caller than main — it does not exist. Kept as a predicate over the
/// label rather than inlined into the gate so the rule is checkable without a
/// live webview.
fn is_allowed_caller(label: &str) -> bool {
    label == MAIN_WINDOW_LABEL
}

fn require_caller(caller: &WebviewWindow) -> Result<(), AgentPanelCommandErrorV1> {
    if is_allowed_caller(caller.label()) {
        Ok(())
    } else {
        Err(AgentPanelCommandErrorV1::UnauthorizedWindow)
    }
}

#[tauri::command]
#[specta::specta]
pub fn agent_panel_status(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    Ok(manager.status())
}

#[tauri::command]
#[specta::specta]
pub async fn agent_panel_send_turn(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
    request: AgentPanelSendTurnRequestV1,
) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager.send_turn(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn agent_panel_cancel_turn(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
    request: AgentPanelCancelTurnRequestV1,
) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager.cancel_turn(request).await
}

#[tauri::command]
#[specta::specta]
pub fn agent_panel_apply_change(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
    request: AgentPanelApplyChangeRequestV1,
) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager.apply_change(request)
}

#[tauri::command]
#[specta::specta]
pub fn agent_panel_undo_change(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
    request: AgentPanelUndoChangeRequestV1,
) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager.undo_change(request)
}

/// Make one of the answer's offered changes. Nothing here is reachable without
/// a press: there is no setting that applies an action on arrival, and there
/// is not going to be one.
#[tauri::command]
#[specta::specta]
pub async fn agent_panel_apply_action(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    request: AgentPanelActionRequestV1,
) -> Result<AgentPanelTurnStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager
        .apply_action(meetings.inner().as_ref(), request)
        .await
}

/// Refuse one of the answer's offered changes, or reverse it after the fact.
#[tauri::command]
#[specta::specta]
pub async fn agent_panel_dismiss_action(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    request: AgentPanelActionRequestV1,
) -> Result<AgentPanelTurnStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager
        .dismiss_action(meetings.inner().as_ref(), request)
        .await
}

/// The titles the history button lists, newest first.
#[tauri::command]
#[specta::specta]
pub fn agent_chat_history_list(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
) -> Result<Vec<AgentChatConversationSummaryV1>, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    Ok(manager.history_list())
}

#[tauri::command]
#[specta::specta]
pub fn agent_chat_open(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
    conversation_id: String,
) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager.open_conversation(&conversation_id)
}

#[tauri::command]
#[specta::specta]
pub fn agent_chat_new(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
) -> Result<AgentPanelStatusV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager.new_conversation()
}

pub(crate) async fn cli_public_identity(
) -> Result<AgentPanelPublicIdentityV1, AgentPanelCommandErrorV1> {
    let secrets = crate::secrets::SecretManager::native();
    relay::public_identity(true, &secrets)
        .await
        .map_err(map_relay_error)
}

#[tauri::command]
#[specta::specta]
pub async fn agent_panel_public_identity(
    caller: WebviewWindow,
    manager: State<'_, AgentPanelManager>,
) -> Result<AgentPanelPublicIdentityV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    manager.public_identity().await
}

/// Switching the agent off stops the poll loop with it.
///
/// A loop left running would keep signing requests to a relay the reader has
/// just said no to, and would keep doing it until the turn finished. Nothing
/// else needs closing: the sheet is a fold in the main window's layout, and
/// the pill that opens it disappears with the setting.
#[tauri::command]
#[specta::specta]
pub fn change_agent_panel_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    crate::settings::update_settings(&app, |settings| {
        settings.agent_panel_enabled = enabled;
    })?;
    if !enabled {
        if let Some(manager) = app.try_state::<AgentPanelManager>() {
            manager.stop_polling();
        }
    }
    Ok(())
}

/// Where the pairing lives, read back for a receipt. Settings is the one copy
/// of this state; nothing caches it beside the store.
fn pairing_status(app: &AppHandle) -> AgentPanelPairingStatusV1 {
    let settings = crate::settings::get_settings(app);
    AgentPanelPairingStatusV1 {
        paired: settings.agent_panel_paired,
        relay_url: settings.agent_panel_relay_url,
        relay_key_id: settings.agent_panel_relay_key_id,
        relay_public_key: settings.agent_panel_relay_public_key,
        last_successful_connection_at_utc_ms: settings.agent_panel_last_successful_connection_at,
    }
}

fn pairing_receipt(
    app: &AppHandle,
    command: AgentPanelPairingCommandV1,
    requested_at_utc_ms: i64,
) -> Result<AgentPanelPairingReceiptV1, AgentPanelCommandErrorV1> {
    let receipt_id = format!(
        "pairing-{}",
        relay::new_idempotency_key().map_err(map_relay_error)?
    );
    Ok(AgentPanelPairingReceiptV1 {
        schema_version: 1,
        receipt_id,
        command,
        actor: AgentPanelActorV1::User,
        requested_at_utc_ms,
        committed_at_utc_ms: chrono::Utc::now().timestamp_millis(),
        pairing: pairing_status(app),
    })
}

/// Pair the panel with a relay. The URL and the pinned key are checked by the
/// same code the client uses to build a request, so a pairing that saves is a
/// pairing the next turn can actually use.
///
/// Pairing does not test the connection — `agent_panel_test_connection` does,
/// and separating them is what lets the screen say "saved, never reached"
/// instead of refusing to save a relay that happens to be asleep.
#[tauri::command]
#[specta::specta]
pub fn set_agent_panel_pairing(
    caller: WebviewWindow,
    app: AppHandle,
    request: AgentPanelPairingRequestV1,
) -> Result<AgentPanelPairingReceiptV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    let requested_at_utc_ms = chrono::Utc::now().timestamp_millis();
    let pairing = validate_pairing(
        &request.relay_url,
        &request.relay_key_id,
        &request.relay_public_key,
    )
    .map_err(map_relay_error)?;
    crate::settings::update_settings(&app, |settings| {
        let rotated = settings.agent_panel_relay_url.as_deref() != Some(pairing.relay_url.as_str())
            || settings.agent_panel_relay_public_key.as_deref()
                != Some(pairing.relay_public_key.as_str());
        settings.agent_panel_relay_url = Some(pairing.relay_url);
        settings.agent_panel_relay_key_id = Some(pairing.relay_key_id);
        settings.agent_panel_relay_public_key = Some(pairing.relay_public_key);
        settings.agent_panel_paired = true;
        /* A different relay or a different key is a different peer, so the
         * last time we reached one says nothing about this one. */
        if rotated {
            settings.agent_panel_last_successful_connection_at = None;
        }
    })
    .map_err(|_| AgentPanelCommandErrorV1::ActionFailed)?;
    pairing_receipt(&app, AgentPanelPairingCommandV1::Set, requested_at_utc_ms)
}

#[tauri::command]
#[specta::specta]
pub fn clear_agent_panel_pairing(
    caller: WebviewWindow,
    app: AppHandle,
) -> Result<AgentPanelPairingReceiptV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    let requested_at_utc_ms = chrono::Utc::now().timestamp_millis();
    crate::settings::update_settings(&app, |settings| {
        settings.agent_panel_relay_url = None;
        settings.agent_panel_relay_key_id = None;
        settings.agent_panel_relay_public_key = None;
        settings.agent_panel_paired = false;
        settings.agent_panel_last_successful_connection_at = None;
    })
    .map_err(|_| AgentPanelCommandErrorV1::ActionFailed)?;
    pairing_receipt(&app, AgentPanelPairingCommandV1::Clear, requested_at_utc_ms)
}

/// One signed round-trip against the paired relay, so the screen can tell a
/// wrong URL from a wrong key from a relay that is simply not running.
#[tauri::command]
#[specta::specta]
pub async fn agent_panel_test_connection(
    caller: WebviewWindow,
    app: AppHandle,
    manager: State<'_, AgentPanelManager>,
) -> Result<AgentPanelPairingReceiptV1, AgentPanelCommandErrorV1> {
    require_caller(&caller)?;
    let requested_at_utc_ms = chrono::Utc::now().timestamp_millis();
    manager.test_connection().await?;
    crate::settings::update_settings(&app, |settings| {
        settings.agent_panel_last_successful_connection_at = Some(requested_at_utc_ms);
    })
    .map_err(|_| AgentPanelCommandErrorV1::ActionFailed)?;
    pairing_receipt(
        &app,
        AgentPanelPairingCommandV1::TestConnection,
        requested_at_utc_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Theme;

    #[test]
    fn confirmation_uses_the_strictest_action() {
        assert_eq!(
            strongest_confirmation(&[
                SonaSettingChangeV1::Theme(Theme::Dark),
                SonaSettingChangeV1::LocalRetentionPeriod(3),
            ]),
            SonaConfirmationClassV1::Explicit
        );
    }

    #[test]
    fn opaque_turn_ids_reject_paths_and_urls() {
        assert!(is_opaque_id("turn-0123"));
        assert!(!is_opaque_id("/tmp/turn"));
        assert!(!is_opaque_id("https://turn"));
    }

    #[test]
    fn terminal_turns_do_not_poll() {
        assert!(AgentPanelTurnStateV1::Succeeded.is_terminal());
        assert!(!AgentPanelTurnStateV1::Running.is_terminal());
    }

    fn outcome(id: &str, tool: &str, ok: bool, result: &str) -> ToolResult {
        ToolResult {
            id: id.to_string(),
            tool: tool.to_string(),
            ok,
            result: result.to_string(),
            sources: Vec::new(),
        }
    }

    /// The block is the shape the relay's prompt tells the model to expect:
    /// one header, then a `call` line and a body per lookup.
    #[test]
    fn a_round_of_results_is_appended_the_way_the_prompt_describes() {
        let calls = vec![
            ToolCall {
                id: "c1".to_string(),
                tool: "word_stats".to_string(),
                args: serde_json::json!({"days": 90}),
            },
            ToolCall {
                id: "c2".to_string(),
                tool: "search".to_string(),
                args: serde_json::json!({"query": "de\nck"}),
            },
        ];
        let results = vec![
            outcome("c1", "word_stats", true, r#"{"total_words":96410}"#),
            outcome("c2", "search", false, "unknown tool"),
        ];

        let pack = append_tool_block("sona corpus card 1\nnow: x", 1, &calls, &results);

        assert_eq!(
            pack,
            "sona corpus card 1\nnow: x\n\n\
             tool results round 1 of 3\n\
             call c1 word_stats {\"days\":90} ok\n\
             {\"total_words\":96410}\n\
             call c2 search {\"query\":\"de\\nck\"} error\n\
             unknown tool"
        );
        assert!(
            !pack
                .chars()
                .any(|character| character.is_control() && character != '\n'),
            "a newline inside an argument is escaped, not written"
        );
        assert_eq!(
            append_tool_block("", 2, &calls[..1], &results[..1]),
            "tool results round 2 of 3\ncall c1 word_stats {\"days\":90} ok\n{\"total_words\":96410}",
            "a turn that sent no pack gets the block alone"
        );
    }

    /// The panel's ceiling holds over the grown pack. Results are cut from
    /// the last call backwards, each replaced by a line that says so, and the
    /// header says the block was cut.
    #[test]
    fn a_block_that_does_not_fit_is_cut_from_its_last_result_and_says_so() {
        let base = "x".repeat(MAX_CONTEXT_PACK_BYTES - 200);
        let calls = vec![
            ToolCall {
                id: "c1".to_string(),
                tool: "recent".to_string(),
                args: serde_json::json!({}),
            },
            ToolCall {
                id: "c2".to_string(),
                tool: "activity".to_string(),
                args: serde_json::json!({}),
            },
        ];
        let results = vec![
            outcome("c1", "recent", true, &"r".repeat(60)),
            outcome("c2", "activity", true, &"a".repeat(500)),
        ];

        let pack = append_tool_block(&base, 3, &calls, &results);

        assert!(pack.len() <= MAX_CONTEXT_PACK_BYTES, "{} bytes", pack.len());
        assert!(pack.starts_with(&base));
        assert!(pack.contains("\n\ntool results round 3 of 3 (cut)\n"));
        assert!(pack.contains(&format!("\ncall c1 recent {{}} ok\n{}\n", "r".repeat(60))));
        assert!(pack.ends_with("\ncall c2 activity {} ok\nresult cut at the pack ceiling"));

        let full = "x".repeat(MAX_CONTEXT_PACK_BYTES);
        assert_eq!(
            append_tool_block(&full, 1, &calls, &results),
            full,
            "a pack with no room for even the header is left alone"
        );
    }

    #[test]
    fn a_lookup_step_is_keyed_by_round_and_position() {
        assert_eq!(tool_step_id(1, 0), "tool-1-0");
        assert_ne!(tool_step_id(1, 0), tool_step_id(2, 0));
        assert_ne!(tool_step_id(1, 0), tool_step_id(1, 1));
    }

    /// The gate every command on this surface goes through.
    ///
    /// It widened when the chat moved inside the main window: the commands
    /// that drive a turn used to be reachable only from the companion
    /// webview, and are now reachable only from main. What must not widen with
    /// it is everything else — the overlay, the consent window, and the label
    /// the deleted panel used to answer to.
    #[test]
    fn only_the_main_window_reaches_the_agent_commands() {
        assert!(is_allowed_caller("main"));
        for label in ["agent-panel", "recording_overlay", "consent", ""] {
            assert!(!is_allowed_caller(label), "{label} is not the main window");
        }
    }

    fn reported(id: &str, state: SonaAgentStepStateV1) -> SonaAgentStepV1 {
        SonaAgentStepV1 {
            id: id.to_string(),
            label: format!("step {id}"),
            state,
        }
    }

    /// The relay resends its whole step list every poll and dates none of it,
    /// so the first sighting is the only start a row can honestly claim, and it
    /// must survive every later poll that repeats the same step.
    #[test]
    fn a_steps_start_is_the_poll_that_first_reported_it() {
        let mut held = Vec::new();
        merge_steps(
            &mut held,
            &[reported("read", SonaAgentStepStateV1::Running)],
            400,
        );
        merge_steps(
            &mut held,
            &[
                reported("read", SonaAgentStepStateV1::Running),
                reported("draft", SonaAgentStepStateV1::Running),
            ],
            1_150,
        );

        assert_eq!(held.len(), 2);
        assert_eq!(held[0].started_after_ms, 400);
        assert_eq!(held[0].ended_after_ms, None);
        assert_eq!(held[1].started_after_ms, 1_150);
    }

    /// A step that stops running is dated once, at the poll that saw it stop.
    #[test]
    fn a_finished_step_keeps_the_offset_it_finished_at() {
        let mut held = Vec::new();
        merge_steps(
            &mut held,
            &[reported("read", SonaAgentStepStateV1::Running)],
            400,
        );
        merge_steps(
            &mut held,
            &[reported("read", SonaAgentStepStateV1::Done)],
            900,
        );
        merge_steps(
            &mut held,
            &[reported("read", SonaAgentStepStateV1::Done)],
            4_000,
        );

        assert_eq!(held[0].state, SonaAgentStepStateV1::Done);
        assert_eq!(held[0].ended_after_ms, Some(900));
    }

    /// A response that stops mentioning a step has not un-run it. The rail is
    /// a record of what happened, so a shrinking list must not shorten it.
    #[test]
    fn a_step_the_relay_stops_reporting_is_still_on_the_rail() {
        let mut held = Vec::new();
        merge_steps(
            &mut held,
            &[reported("read", SonaAgentStepStateV1::Done)],
            400,
        );
        merge_steps(
            &mut held,
            &[reported("draft", SonaAgentStepStateV1::Running)],
            900,
        );

        assert_eq!(
            held.iter().map(|step| step.id.as_str()).collect::<Vec<_>>(),
            vec!["read", "draft"]
        );
    }

    /// A caller outside the panel decides what to tell its reader from this
    /// classification alone, so the line between the two groups is the whole
    /// contract: anything that means "nothing ran on the far side" must not
    /// arrive as a failed attempt, and an answer that came back wrong must not
    /// arrive as an unreachable server.
    #[test]
    fn unreached_relays_are_not_reported_as_failed_answers() {
        for error in [
            RelayError::Disabled,
            RelayError::Unpaired,
            RelayError::InvalidConfiguration,
            RelayError::CleartextRejected,
            RelayError::SecretUnavailable,
            RelayError::RandomUnavailable,
            RelayError::RequestFailed,
        ] {
            assert_eq!(
                chat_turn_error(error),
                ChatTurnError::Unreachable,
                "{error:?} never reached a relay"
            );
        }
        for error in [
            RelayError::ResponseTooLarge,
            RelayError::ResponseSignatureInvalid,
            RelayError::ResponseMalformed,
            RelayError::RemoteRejected,
            RelayError::OwnershipRejected,
        ] {
            assert_eq!(
                chat_turn_error(error),
                ChatTurnError::Failed,
                "{error:?} is an answer this client refused"
            );
        }
    }

    #[test]
    fn panel_failures_keep_the_reason_the_sheet_can_act_on() {
        assert_eq!(
            turn_failure_for_relay_error(RelayError::RequestFailed),
            AgentPanelTurnFailureV1::Unreachable
        );
        assert_eq!(
            turn_failure_for_relay_error(RelayError::RemoteRejected),
            AgentPanelTurnFailureV1::Refused
        );
        assert_eq!(
            turn_failure_for_relay_error(RelayError::ResponseMalformed),
            AgentPanelTurnFailureV1::Failed
        );
        assert_eq!(
            turn_failure_for_job(RelayJobFailure::Refused),
            AgentPanelTurnFailureV1::Refused
        );
        assert_eq!(
            turn_failure_for_job(RelayJobFailure::Failed),
            AgentPanelTurnFailureV1::Failed
        );
    }

    fn prose() -> SonaAgentResponseV1 {
        SonaAgentResponseV1::Text {
            message: "The deck ships Thursday.".to_string(),
            actions: Vec::new(),
            steps: Vec::new(),
        }
    }

    fn settings_proposal() -> SonaAgentResponseV1 {
        SonaAgentResponseV1::Proposal {
            proposal: SonaConfigProposalV1 {
                version: protocol::SONA_CONFIG_PROPOSAL_VERSION.to_string(),
                summary: "Use dark mode.".to_string(),
                rationale: "It is the requested local setting.".to_string(),
                actions: vec![SonaSettingChangeV1::Theme(Theme::Dark)],
                follow_up_question: None,
                source_settings_revision: 7,
            },
            steps: Vec::new(),
        }
    }

    /// An answer on the workspace that did not ask is the relay's own bug, and
    /// the reader is owed that rather than "this Mac could not verify it" —
    /// which is what the shape used to decide, wrongly, in both directions: a
    /// crossed proposal read as a malformed one, crossed prose as unverified.
    #[test]
    fn a_crossed_workspace_is_not_an_unverified_answer() {
        for response in [prose(), settings_proposal()] {
            assert_eq!(
                refusal_for_validation(ProposalValidationError::WorkspaceMismatch, &response),
                RelayJobRefusal::WorkspaceMismatch,
                "{response:?} crossed the workspace line"
            );
        }
        assert_eq!(
            refusal_for_validation(ProposalValidationError::InvalidAssistantMessage, &prose()),
            RelayJobRefusal::UntrustedResponse
        );
        assert_eq!(
            refusal_for_validation(
                ProposalValidationError::InvalidSettingValue,
                &settings_proposal()
            ),
            RelayJobRefusal::InvalidProposal
        );
    }

    /// The poll loop records the status a second time, after the refusal was
    /// already named. Flattening every refusal onto one row is how the
    /// distinction died on the path that actually carries answers.
    #[test]
    fn the_poll_loop_keeps_the_status_the_refusal_earned() {
        assert_eq!(
            protocol_failure_status(AgentPanelCommandErrorV1::WorkspaceMismatch),
            AgentPanelRelayStatusV1::WorkspaceMismatch
        );
        for error in [
            AgentPanelCommandErrorV1::UntrustedResponse,
            AgentPanelCommandErrorV1::InvalidProposal,
            AgentPanelCommandErrorV1::Offline,
        ] {
            assert_eq!(
                protocol_failure_status(error),
                AgentPanelRelayStatusV1::UntrustedResponse,
                "{error:?} is not a workspace mismatch"
            );
        }
    }
}

use super::protocol::{
    AgentPanelTurnFailureV1, AgentPanelWorkspaceV1, SonaAgentChatTurnV1, SonaAgentStepStateV1,
    SonaChatActionV1, SonaConfirmationClassV1, SonaSettingChangeV1,
};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanelRelayStatusV1 {
    Disabled,
    Unpaired,
    Ready,
    RateLimited,
    Offline,
    InvalidConfiguration,
    SecretUnavailable,
    UntrustedResponse,
    /// A signed answer that did not fit the turn it answered: a settings
    /// proposal for an Ask turn, or prose for a Configure one. The signature
    /// held; the shape did not.
    WorkspaceMismatch,
    RemoteRejected,
    OwnershipRejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanelTurnStateV1 {
    Submitting,
    Queued,
    Leased,
    Running,
    WaitingUser,
    WaitingApproval,
    Canceling,
    Succeeded,
    Failed,
    Canceled,
    UnverifiedExternal,
}

impl AgentPanelTurnStateV1 {
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Canceled | Self::UnverifiedExternal
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanelProposalStateV1 {
    Pending,
    Applied,
    Undone,
    Rejected,
}

/// Where one offered corpus change has got to.
///
/// Three states rather than the proposal's four: an action that has been
/// undone is an action that is not in effect, which is what `Dismissed`
/// already means, and a second word for it would be a second thing for the
/// card to explain. Pressing Dismiss on a pending action and Undo on an
/// applied one is therefore the same command and the same destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanelActionStateV1 {
    Pending,
    Applied,
    Dismissed,
}

/// One card under an answer: what the assistant offered to change, whether it
/// has happened, and the receipt it produced.
///
/// `operation_id` is the [`crate::meeting::types::OperationReceipt`] the
/// mutation recorded, so the change can be found in the ledger beside every
/// other change to the same meeting. It is `None` for a vocabulary term: that
/// write goes to settings, which keeps no operation ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelActionV1 {
    /// Position in the turn's offer, which is how a command names one.
    pub action_index: u32,
    pub action: SonaChatActionV1,
    pub state: AgentPanelActionStateV1,
    pub operation_id: Option<String>,
}

/// One row of the sheet's "Worked for Ns" disclosure.
///
/// The relay reports a step's identity, its label and whether it is still
/// going; it reports no time at all. The panel is the only clock either side
/// has, so the two offsets here are what the panel itself observed, measured
/// from [`AgentPanelTurnStatusV1::started_at_utc_ms`] — first sighting, and
/// the poll at which the step stopped running. Offsets rather than wall clocks
/// because a duration is what the row shows, and a pair of offsets cannot
/// disagree with the turn they belong to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelStepV1 {
    pub id: String,
    pub label: String,
    pub state: SonaAgentStepStateV1,
    pub started_after_ms: i64,
    /// `None` while the step is still running.
    pub ended_after_ms: Option<i64>,
    /// The Sona tool this step ran, for a step the panel made itself while
    /// answering a `tool_calls` reply. The sheet names it in the reader's
    /// language; `label` carries the tool name as the fallback. `None` on a
    /// step the relay reported.
    pub tool: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelTurnStatusV1 {
    pub turn_id: String,
    pub workspace: AgentPanelWorkspaceV1,
    pub state: AgentPanelTurnStateV1,
    pub event_cursor: u64,
    /// When the panel accepted this turn, so the sheet can count elapsed time
    /// without inventing a start of its own every time it re-reads status.
    pub started_at_utc_ms: i64,
    /// When it reached a terminal state. A finished turn's "Worked for Ns" is
    /// a fact about the past, so it is fixed here rather than left to whatever
    /// the reader's clock says the next time the sheet is opened.
    pub completed_at_utc_ms: Option<i64>,
    /// What the remote side did on the way to its answer. Empty until a
    /// workspace reports steps.
    pub steps: Vec<AgentPanelStepV1>,
    /// What the answer offered to change in the corpus, in the order it
    /// offered them. Empty unless the reader asked for a change.
    pub actions: Vec<AgentPanelActionV1>,
    /// Why it has no answer, when the panel can name the reason.
    pub failure: Option<AgentPanelTurnFailureV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelProposalPreviewV1 {
    pub proposal_id: String,
    pub summary: String,
    pub rationale: String,
    pub actions: Vec<SonaSettingChangeV1>,
    pub follow_up_question: Option<String>,
    pub source_settings_revision: u64,
    pub confirmation: SonaConfirmationClassV1,
    pub state: AgentPanelProposalStateV1,
    pub receipt_id: Option<String>,
    pub applied_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelStatusV1 {
    pub invalidation_id: u64,
    pub relay_status: AgentPanelRelayStatusV1,
    pub conversation_id: Option<String>,
    pub conversation: Vec<SonaAgentChatTurnV1>,
    pub turn: Option<AgentPanelTurnStatusV1>,
    pub proposal: Option<AgentPanelProposalPreviewV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelSendTurnRequestV1 {
    pub turn_id: String,
    pub message: String,
    pub locale: String,
    pub workspace: AgentPanelWorkspaceV1,
    /// Evidence for this one question: quotes, ids and `sona://` links, built
    /// by whoever is asking. The panel does not assemble packs, and a turn
    /// without one is an ordinary question.
    pub context_pack: Option<String>,
    /// Whether this one turn may reach the operator's own MCP servers. Off
    /// unless the reader turned it on for this send.
    pub tools_allowed: bool,
}

/// Apply, or put back, one of a turn's offered changes.
///
/// The index is the card's position in the offer rather than an id of its
/// own: an action has no existence apart from the turn that proposed it, and
/// minting an id for it would invite a caller to hold one past the turn's
/// life.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelActionRequestV1 {
    pub turn_id: String,
    pub action_index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelCancelTurnRequestV1 {
    pub turn_id: String,
}

/// Apply one proposal, whole.
///
/// No action index: the card offers the change set the proposal describes, and
/// the receipt it produces covers all of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelApplyChangeRequestV1 {
    pub proposal_id: String,
    pub expected_revision: u64,
    pub confirmed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelUndoChangeRequestV1 {
    pub receipt_id: String,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelPairingRequestV1 {
    pub relay_url: String,
    pub relay_key_id: String,
    pub relay_public_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanelPairingCommandV1 {
    Set,
    Clear,
    TestConnection,
}

/// What the panel is paired to, as the settings store holds it. The private
/// half of this machine's identity is never here — it stays in the secret
/// backend, and `agent_panel_public_identity` is how the relay learns the
/// public half.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelPairingStatusV1 {
    pub paired: bool,
    pub relay_url: Option<String>,
    pub relay_key_id: Option<String>,
    pub relay_public_key: Option<String>,
    pub last_successful_connection_at_utc_ms: Option<i64>,
}

/// Proof that a pairing change happened, in the shape the rest of the app
/// uses: what was asked, by whom, when it committed, and the state it left
/// behind. A refused change returns `AgentPanelCommandErrorV1` instead — the
/// error is the reason code, and nothing was written to have a receipt for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelPairingReceiptV1 {
    pub schema_version: u32,
    pub receipt_id: String,
    pub command: AgentPanelPairingCommandV1,
    pub actor: AgentPanelActorV1,
    pub requested_at_utc_ms: i64,
    pub committed_at_utc_ms: i64,
    pub pairing: AgentPanelPairingStatusV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanelActorV1 {
    User,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanelCommandErrorV1 {
    UnauthorizedWindow,
    Disabled,
    Unpaired,
    Offline,
    InvalidConfiguration,
    SecretUnavailable,
    UntrustedResponse,
    /// The reply was signed and then did not fit the turn it answered, so
    /// nothing in it was taken. A shape mismatch, not a signature failure.
    WorkspaceMismatch,
    RemoteRejected,
    OwnershipRejected,
    UnknownConversation,
    InvalidRequest,
    TurnActive,
    UnknownTurn,
    UnknownProposal,
    /// No card at that index on that turn.
    UnknownAction,
    /// The mutation behind a card refused: the meeting moved under it, the row
    /// it named is gone, or the store would not take the write.
    ActionFailed,
    ConfirmationRequired,
    StaleProposal,
    InvalidProposal,
    InvalidSetting,
    NotUndoable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelStatusChangedEvent {
    pub invalidation_id: u64,
    pub status: AgentPanelRelayStatusV1,
}

impl tauri_specta::Event for AgentPanelStatusChangedEvent {
    const NAME: &'static str = "agent-panel://status-changed";
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelTurnChangedEvent {
    pub invalidation_id: u64,
    pub turn_id: Option<String>,
    pub state: Option<AgentPanelTurnStateV1>,
}

impl tauri_specta::Event for AgentPanelTurnChangedEvent {
    const NAME: &'static str = "agent-panel://turn-changed";
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelProposalChangedEvent {
    pub invalidation_id: u64,
    pub proposal_id: Option<String>,
    pub state: Option<AgentPanelProposalStateV1>,
}

impl tauri_specta::Event for AgentPanelProposalChangedEvent {
    const NAME: &'static str = "agent-panel://proposal-changed";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_keep_the_frozen_panel_names() {
        assert_eq!(
            <AgentPanelStatusChangedEvent as tauri_specta::Event>::NAME,
            "agent-panel://status-changed"
        );
        assert_eq!(
            <AgentPanelTurnChangedEvent as tauri_specta::Event>::NAME,
            "agent-panel://turn-changed"
        );
        assert_eq!(
            <AgentPanelProposalChangedEvent as tauri_specta::Event>::NAME,
            "agent-panel://proposal-changed"
        );
    }
}

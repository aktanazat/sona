import type {
  AgentPanelProposalPreviewV1,
  AgentPanelStatusV1,
  AgentPanelStepV1,
  AgentPanelTurnStatusV1,
  AgentPanelWorkspaceV1,
  SonaAgentChatTurnV1,
  SonaChatActionV1,
} from "@/bindings";

/**
 * One line saying what a card will change, as a translation key and the values
 * it interpolates.
 *
 * The line names the kind of change rather than the row it touches: a loop id
 * is a digest, and printing one in a 340pt column would tell the reader
 * nothing. What tells them which loop is the action's own reason, drawn
 * underneath, which is a sentence about that one commitment.
 *
 * `translate` is the caller's `t`, because one value — the notes template — is
 * itself a name the catalogue owns, and it is the same name the meetings
 * screens show.
 */
export interface ActionLine {
  key: string;
  values: Record<string, string>;
}

export const actionLine = (
  action: SonaChatActionV1,
  translate: (key: string) => string,
): ActionLine => {
  switch (action.kind) {
    case "set_series_template":
      return {
        key: "chat.action.set_series_template",
        values: {
          template: translate(`meetings.notes.templates.${action.template_id}`),
        },
      };
    case "add_vocabulary_term":
      return {
        key: "chat.action.add_vocabulary_term",
        values: { term: action.replacement ?? action.term },
      };
    case "rename_speaker":
      return {
        key: "chat.action.rename_speaker",
        values: { name: action.name },
      };
    default:
      return { key: `chat.action.${action.kind}`, values: {} };
  }
};

/**
 * What the relay is doing, as the sheet needs to know it.
 *
 * Eleven relay statuses collapse onto seven, because the sheet acts on exactly
 * seven things: it has not asked yet, the agent is off, it is unpaired, the
 * relay is away, the relay is asking it to slow down, something else went
 * wrong, or it works. Whether a turn is running and whether a proposal is on
 * screen are separate facts read from the status itself.
 */
export type ChatPhase =
  | "loading"
  | "disabled"
  | "unpaired"
  | "offline"
  | "rate_limited"
  | "error"
  | "ready";

/** The phases that owe the reader a sentence instead of a conversation. */
export const CHAT_NOTICE_PHASES = {
  disabled: true,
  unpaired: true,
  offline: true,
  rate_limited: true,
  error: true,
} satisfies Partial<Record<ChatPhase, true>>;

export const chatPhase = (status: AgentPanelStatusV1 | null): ChatPhase => {
  if (status === null) return "loading";
  switch (status.relay_status) {
    case "disabled":
      return "disabled";
    case "unpaired":
      return "unpaired";
    case "offline":
      return "offline";
    case "rate_limited":
      return "rate_limited";
    case "ready":
      return "ready";
    /* Invalid pairing, a missing secret, an answer that failed verification or
     * came back for the workspace that did not ask, a rejection from the far
     * side: all of them mean the same thing to someone looking at a chat
     * window — it is not going to answer, and Settings is where the pairing
     * lives. */
    default:
      return "error";
  }
};

/**
 * The turn states that are over. A live turn can be stopped; a finished one is
 * a record, and offering to stop it is offering to undo the past.
 */
const TERMINAL_TURN_STATES = {
  succeeded: true,
  failed: true,
  canceled: true,
  unverified_external: true,
} satisfies Partial<Record<AgentPanelTurnStatusV1["state"], true>>;

/**
 * Whether the turn on screen is still going. Deliberately not a type guard:
 * its negation means "finished OR absent", and a guard would narrow the
 * finished case away.
 */
export const isTurnRunning = (turn: AgentPanelTurnStatusV1 | null): boolean =>
  turn !== null && !(turn.state in TERMINAL_TURN_STATES);

export interface ChatPackGate {
  paired: boolean;
  remoteIntelligence: boolean;
}

/** Corpus evidence leaves this Mac only for the Ask workspace under its gate. */
export const shouldPackChatTurn = (
  workspace: AgentPanelWorkspaceV1,
  gate: ChatPackGate,
): boolean =>
  workspace === "sona_chat" && gate.paired && gate.remoteIntelligence;

/**
 * The one thing standing between an Ask turn and the corpus is the consent
 * switch. A paired sheet whose reader has not thrown it gets a notice with the
 * switch in it; an unpaired one has a different problem, named by its phase.
 */
export const needsRemoteConsent = (
  workspace: AgentPanelWorkspaceV1,
  gate: ChatPackGate,
): boolean =>
  workspace === "sona_chat" && gate.paired && !gate.remoteIntelligence;

export type ChatTurnFailure =
  | "unreachable"
  | "refused"
  | "failed"
  | "too_many_lookups"
  | "rate_limited";

/** The durable, localized failure category carried by a terminal turn. */
export const turnFailure = (
  turn: AgentPanelTurnStatusV1 | null,
): ChatTurnFailure | null => turn?.failure ?? null;

/**
 * How long the turn took, in milliseconds.
 *
 * A finished turn's number is fixed by the backend, so reopening the sheet
 * tomorrow still says how long it took rather than how long ago it was.
 */
export const workedMs = (turn: AgentPanelTurnStatusV1, now: number): number =>
  Math.max(0, (turn.completed_at_utc_ms ?? now) - turn.started_at_utc_ms);

/** The wait message is a UI promise, never a relay timeout. */
export const STILL_WAITING_AFTER_MS = 30_000;

export const isStillWaiting = (
  turn: AgentPanelTurnStatusV1 | null,
  now: number,
): boolean =>
  turn !== null &&
  turn.steps.length === 0 &&
  (turn.state === "queued" || turn.state === "running") &&
  workedMs(turn, now) >= STILL_WAITING_AFTER_MS;

/** How long one step took, on the same axis. */
export const stepMs = (
  step: AgentPanelStepV1,
  turn: AgentPanelTurnStatusV1,
  now: number,
): number =>
  Math.max(
    0,
    (step.ended_after_ms ?? workedMs(turn, now)) - step.started_after_ms,
  );

/**
 * Where the turn's activity belongs in the scrollback: above the answer it
 * produced, or at the end while there is no answer yet.
 *
 * A completed turn with recorded timing needs a row even without steps. A
 * failed turn does too: its only visible reply is the error beneath the
 * question. The optional corpus marker takes that same row.
 */
export const workRowIndex = (
  conversation: readonly SonaAgentChatTurnV1[],
  turn: AgentPanelTurnStatusV1 | null,
  searchedCorpus = false,
): number => {
  if (turn === null) return -1;
  const hasActivity =
    isTurnRunning(turn) ||
    turn.completed_at_utc_ms !== null ||
    turnFailure(turn) !== null ||
    searchedCorpus;
  if (!hasActivity) return -1;
  const last = conversation.length - 1;
  return last >= 0 && conversation[last].role === "assistant"
    ? last
    : conversation.length;
};

/**
 * The scrollback row a live proposal owns, or `-1` if it has none.
 *
 * The backend pushes a proposal's summary onto the conversation and stores the
 * proposal beside it, so the card and the row are the same utterance. Drawing
 * both would print one sentence twice; this is how the card takes the row's
 * place instead of sitting under it.
 */
export const proposalRowIndex = (
  conversation: readonly SonaAgentChatTurnV1[],
  proposal: AgentPanelProposalPreviewV1 | null,
): number => {
  if (proposal === null) return -1;
  const last = conversation.length - 1;
  return last >= 0 &&
    conversation[last].role === "assistant" &&
    conversation[last].message === proposal.summary
    ? last
    : -1;
};

/**
 * Stable keys for a conversation that can legitimately repeat itself: the same
 * role saying the same words twice is one exchange, not one row rendered
 * twice, so the occurrence count is part of the identity.
 */
export const conversationRows = (
  conversation: readonly SonaAgentChatTurnV1[],
): Array<{ key: string; turn: SonaAgentChatTurnV1 }> => {
  const occurrences = new Map<string, number>();
  return conversation.map((turn) => {
    const identity = JSON.stringify([turn.role, turn.message]);
    const occurrence = occurrences.get(identity) ?? 0;
    occurrences.set(identity, occurrence + 1);
    return { key: JSON.stringify([turn.role, turn.message, occurrence]), turn };
  });
};

/* An answer worth reading cites where it came from, and the pack it was given
 * is nothing but quotes with `sona://` addresses beside them (`query/pack.rs`),
 * so an assistant that answers from evidence writes those addresses into its
 * reply. Left as text they are unclickable noise; split out here they are the
 * one gesture that turns an answer back into the meeting it came from.
 *
 * The address stops at the first character that cannot be inside one. Trailing
 * sentence punctuation is trimmed after the fact rather than excluded from the
 * class, because a `?` can legitimately open a query string while a `.` at the
 * end of a sentence never belongs to the link. */
const SONA_LINK = /sona:\/\/[^\s<>"'`)\]]+/g;
const TRAILING_PUNCTUATION = /[.,;:!?]+$/;

export type MessageSegment = { text: string } | { link: string };

/** One message as alternating prose and addresses, in the order it was written. */
export const linkifySona = (message: string): MessageSegment[] => {
  const segments: MessageSegment[] = [];
  let cursor = 0;
  for (const match of message.matchAll(SONA_LINK)) {
    const link = match[0].replace(TRAILING_PUNCTUATION, "");
    if (link === "sona://") continue;
    const start = match.index;
    if (start > cursor) segments.push({ text: message.slice(cursor, start) });
    segments.push({ link });
    cursor = start + link.length;
  }
  if (cursor < message.length) segments.push({ text: message.slice(cursor) });
  return segments;
};

/**
 * The two members a key handler reads. React's own events satisfy this
 * structurally, so a test can build one as a plain object instead of
 * impersonating React's type.
 */
export interface ChatKeyEvent {
  readonly key: string;
  readonly shiftKey: boolean;
  preventDefault(): void;
}

/**
 * Enter sends, Shift+Enter opens a line. The field is a textarea so a question
 * can run to two lines; that must not cost it the way every other single-line
 * field on this machine is sent.
 */
export const composerKeys =
  (send: () => void) =>
  (event: ChatKeyEvent): void => {
    if (event.key !== "Enter" || event.shiftKey) return;
    event.preventDefault();
    send();
  };

/**
 * Escape closes the sheet.
 *
 * Bound on the sheet rather than on the window: a palette, a dialog or a
 * popover open over it owns Escape first, and each of those stops the event
 * before it reaches an ancestor. A window-level listener would close the sheet
 * out from under whichever of them the reader was actually dismissing.
 */
export const sheetKeys =
  (close: () => void) =>
  (event: ChatKeyEvent): void => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    close();
  };

import React from "react";
import { useTranslation } from "react-i18next";
import { ChevronRight, SlidersHorizontal } from "lucide-react";
import type {
  AgentPanelActionV1,
  AgentPanelProposalPreviewV1,
  AgentPanelTurnStatusV1,
  SonaAgentChatTurnV1,
} from "@/bindings";
import { Button } from "@/components/vg/button";
import { cn } from "@/lib/cn";
import {
  actionLine,
  conversationRows,
  isStillWaiting,
  isTurnRunning,
  linkifySona,
  proposalRowIndex,
  stepMs,
  turnFailure,
  workedMs,
  workRowIndex,
} from "./chatModel";

interface AssistantTextProps {
  message: string;
  onOpenLink: (link: string) => void;
}

/* An answer is prose on the surface, not a card and not a bubble: it is the
 * thing the sheet exists to show, and putting a container around it would make
 * it look like an aside to something else. Only the addresses inside it are
 * chrome, because only they are pressable. */
const AssistantText: React.FC<AssistantTextProps> = ({
  message,
  onOpenLink,
}) => {
  return (
    <p className="text-[14px] leading-[21px] whitespace-pre-wrap text-gray-1000 [overflow-wrap:anywhere]">
      {linkifySona(message).map((segment, index) =>
        "link" in segment ? (
          <button
            // SAFETY: segments come from one immutable message string, so
            // position is a stable identity here.
            key={`${index}:${segment.link}`}
            type="button"
            data-slot="chat-citation"
            aria-label={segment.link}
            onClick={() => onOpenLink(segment.link)}
            className="underline decoration-gray-alpha-600 underline-offset-2 hover:decoration-gray-1000"
          >
            {segment.link}
          </button>
        ) : (
          <React.Fragment key={`${index}:text`}>{segment.text}</React.Fragment>
        ),
      )}
    </p>
  );
};

interface TurnWorkProps {
  turn: AgentPanelTurnStatusV1;
  now: number;
  searchedCorpus: boolean;
  busy: boolean;
  onStop: () => void;
  onRetry: (message: string) => void;
  retryMessage: string | null;
}

const WORK_LINE =
  "inline-flex items-center gap-1 text-[13px] leading-[18px] text-gray-900";

/**
 * The activity that belongs below a turn's question.
 *
 * A live turn always gets a timing line. A completed turn gets one when the
 * backend recorded its finish. Failure and the corpus marker share this row
 * because neither has an assistant answer of its own to introduce them.
 */
const TurnWork: React.FC<TurnWorkProps> = ({
  turn,
  now,
  searchedCorpus,
  busy,
  onStop,
  onRetry,
  retryMessage,
}) => {
  const { t } = useTranslation();
  /* The disclosure is a button and a list rather than <details>/<summary>.
   *
   * A <summary> is a disclosure to a browser but not to WebKit's
   * accessibility layer, which exposes no press action on it: a live
   * accessibility run could not open this list at all, and the app ships in a
   * WKWebView. `aria-expanded` on a real button is the pattern the rest of
   * the app already uses for a collapsible row (see
   * settings/meetings/MeetingPreviewCard.tsx), and it is what an assistive
   * client can both read and act on.
   *
   * The list stays in the document and is `hidden` while closed rather than
   * unmounted: `hidden` is what takes it out of the accessibility tree and
   * out of the tab order, and keeping the node means the button's
   * `aria-controls` always points at something that exists. */
  const [open, setOpen] = React.useState(false);
  const stepsId = React.useId();
  const running = isTurnRunning(turn);
  const failure = turnFailure(turn);
  const showTiming = running || turn.completed_at_utc_ms !== null;
  const label = running
    ? `${t(`chat.turnState.${turn.state}`)} · ${t("chat.stepSeconds", {
        seconds: Math.round(workedMs(turn, now) / 1000),
      })}`
    : t("chat.workedFor", {
        seconds: Math.round(workedMs(turn, now) / 1000),
      });

  return (
    <div data-slot="chat-work" className="flex flex-col items-start gap-1">
      {showTiming &&
        (turn.steps.length === 0 ? (
          <p role="status" className={WORK_LINE}>
            {label}
          </p>
        ) : (
          <div className="flex flex-col items-start">
            <button
              type="button"
              data-slot="chat-steps-toggle"
              aria-expanded={open}
              aria-controls={stepsId}
              onClick={() => setOpen(!open)}
              className={cn(WORK_LINE, "hover:text-gray-1000")}
            >
              <ChevronRight
                aria-hidden="true"
                className={cn(
                  "size-3 transition-transform motion-reduce:transition-none",
                  open && "rotate-90",
                )}
              />
              {label}
            </button>
            <ol
              id={stepsId}
              hidden={!open}
              className="mt-1.5 flex list-none flex-col gap-1 border-s border-gray-alpha-400 py-0.5 ps-3"
            >
              {turn.steps.map((step) => (
                <li
                  key={step.id}
                  data-slot="chat-step"
                  className="flex items-center gap-2 text-[13px] leading-[18px]"
                >
                  {/* A tool's name is a machine's name, so it reads as one: a
                      mono uppercase chip in a hairline pill with no fill, the
                      reference's treatment for a status word. It replaces the
                      plain sentence the tool used to render as, which was
                      indistinguishable from the model's own "Thought about it"
                      beside it — and those two are not the same kind of event.
                      A step with no tool stays prose, because it is prose. */}
                  {step.tool === null ? (
                    <span
                      className={cn(
                        "min-w-0 flex-1 truncate",
                        step.state === "failed"
                          ? "text-red-900"
                          : "text-gray-900",
                      )}
                    >
                      {step.label}
                    </span>
                  ) : (
                    <span
                      data-slot="chat-tool"
                      className={cn(
                        "min-w-0 flex-1 truncate rounded-full border px-1.5 font-mono text-[12px] leading-4 tracking-[0.08em] uppercase",
                        step.state === "failed"
                          ? "border-red-400 text-red-900"
                          : "border-gray-alpha-400 text-gray-900",
                      )}
                    >
                      {t(`chat.tool.${step.tool}`, {
                        defaultValue: step.label,
                      })}
                    </span>
                  )}
                  <span className="flex-none text-gray-900 tabular-nums">
                    {t("chat.stepSeconds", {
                      seconds: Math.round(stepMs(step, turn, now) / 1000),
                    })}
                  </span>
                </li>
              ))}
            </ol>
          </div>
        ))}
      {searchedCorpus && (
        <p
          data-slot="chat-searched-corpus"
          className="text-[12px] leading-4 text-gray-900"
        >
          {t("chat.working.searchedCorpus")}
        </p>
      )}
      {isStillWaiting(turn, now) && (
        <p
          data-slot="chat-still-waiting"
          role="status"
          className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[13px] leading-[18px] text-gray-900"
        >
          {t("chat.working.stillWaiting")}
          <Button variant="link" size="xs" onClick={onStop} disabled={busy}>
            {t("chat.working.cancel")}
          </Button>
        </p>
      )}
      {failure !== null && (
        <p
          data-slot="chat-turn-error"
          role="status"
          className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[13px] leading-[18px] text-red-900"
        >
          {t(`chat.error.${failure}`)}
          <Button
            variant="link"
            size="xs"
            onClick={() => {
              if (retryMessage !== null) onRetry(retryMessage);
            }}
            disabled={busy}
          >
            {t("chat.retry")}
          </Button>
        </p>
      )}
    </div>
  );
};
interface StoredOutcomeProps {
  outcome: NonNullable<SonaAgentChatTurnV1["outcome"]>;
  busy: boolean;
  message: string;
  onRetry: (message: string) => void;
}

/** The terminal result remembered with an earlier question. */
const StoredOutcome: React.FC<StoredOutcomeProps> = ({
  outcome,
  busy,
  message,
  onRetry,
}) => {
  const { t } = useTranslation();

  if (outcome.kind === "canceled") {
    return (
      <p
        data-slot="chat-turn-outcome"
        role="status"
        className="text-[13px] leading-[18px] text-gray-900"
      >
        {t("chat.turnState.canceled")}
      </p>
    );
  }

  return (
    <p
      data-slot="chat-turn-error"
      role="status"
      className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[13px] leading-[18px] text-red-900"
    >
      {t(`chat.error.${outcome.failure}`)}
      <Button
        variant="link"
        size="xs"
        onClick={() => onRetry(message)}
        disabled={busy}
      >
        {t("chat.retry")}
      </Button>
    </p>
  );
};

interface ProposalCardProps {
  proposal: AgentPanelProposalPreviewV1;
  busy: boolean;
  onApply: () => void;
  onUndo: () => void;
}

/**
 * A settings answer, as the one thing you can do about it.
 *
 * The card IS the assistant's turn — the backend puts the proposal's summary
 * into the conversation and stores the proposal beside it, so drawing both
 * would print one sentence twice. Applying moves this same card to Applied
 * with an Undo, rather than adding a second card that reports on the first.
 */
const ProposalCard: React.FC<ProposalCardProps> = ({
  proposal,
  busy,
  onApply,
  onUndo,
}) => {
  const { t } = useTranslation();

  return (
    <div
      data-slot="chat-proposal"
      className="flex items-center gap-3 rounded-panel border border-gray-alpha-400 bg-background-100 p-3"
    >
      <span
        aria-hidden="true"
        className="grid size-8 flex-none place-items-center rounded-lg bg-gray-alpha-200 text-gray-1000"
      >
        <SlidersHorizontal className="size-4" />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        {/* Wraps rather than truncates. This sentence is the whole of what the
            assistant said — the backend puts the summary in the conversation
            and the card takes that row, so there is no second copy of it to
            read — and a press on Apply under half a sentence is a press on
            something nobody was shown. The column is 340pt wide; prose from a
            relay breaks anywhere, like every other relay sentence here. */}
        <span className="text-[13px] leading-[18px] text-gray-1000 [overflow-wrap:anywhere]">
          {proposal.summary}
        </span>
        {/* Action keys stay verbatim: they are identifiers, and naming the set
            is what makes one Apply honest about what it moves. */}
        <span className="truncate text-[12px] leading-4 text-gray-900">
          {proposal.actions.map((action) => action.key).join(", ")}
        </span>
      </span>
      {proposal.state === "pending" && (
        <Button
          variant="outline"
          size="sm"
          className="flex-none"
          onClick={onApply}
          disabled={busy}
        >
          {t("chat.proposal.apply")}
        </Button>
      )}
      {proposal.state === "applied" && (
        <span className="flex flex-none items-center gap-1.5">
          <span className="text-[13px] leading-[18px] text-gray-900">
            {t("chat.proposal.applied")}
          </span>
          <Button variant="ghost" size="sm" onClick={onUndo} disabled={busy}>
            {t("chat.proposal.undo")}
          </Button>
        </span>
      )}
      {(proposal.state === "undone" || proposal.state === "rejected") && (
        <span className="flex-none text-[13px] leading-[18px] text-gray-900">
          {t(`chat.proposal.${proposal.state}`)}
        </span>
      )}
    </div>
  );
};

interface ChatActionCardProps {
  action: AgentPanelActionV1;
  busy: boolean;
  onApply: () => void;
  onDismiss: () => void;
}

/**
 * One corpus change the answer offered, as the one thing you can do about it.
 *
 * Built like the settings proposal card and read the same way: what changes on
 * the first line, why underneath, one button that makes it happen. Applying
 * moves this same card to Applied with an Undo beside it, rather than adding a
 * second card that reports on the first.
 *
 * Dismiss and Undo are one gesture with two labels — "this change is not in
 * effect" — so both go to the same command, and both leave the card saying
 * Dismissed.
 */
const ChatActionCard: React.FC<ChatActionCardProps> = ({
  action,
  busy,
  onApply,
  onDismiss,
}) => {
  const { t } = useTranslation();
  const line = actionLine(action.action, t);

  return (
    <div
      data-slot="chat-action"
      className="flex items-center gap-3 rounded-panel border border-gray-alpha-400 bg-background-100 p-3"
    >
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        {/* Wraps rather than truncates, for the reason the proposal card
            wraps: a press on Apply under half a sentence is a press on
            something nobody was shown. */}
        <span className="text-[13px] leading-[18px] text-gray-1000 [overflow-wrap:anywhere]">
          {t(line.key, line.values)}
        </span>
        <span className="text-[12px] leading-4 text-gray-900 [overflow-wrap:anywhere]">
          {action.action.reason}
        </span>
      </span>
      {action.state === "pending" && (
        <span className="flex flex-none items-center gap-1.5">
          <Button variant="outline" size="sm" onClick={onApply} disabled={busy}>
            {t("chat.action.apply")}
          </Button>
          <Button variant="ghost" size="sm" onClick={onDismiss} disabled={busy}>
            {t("chat.action.dismiss")}
          </Button>
        </span>
      )}
      {action.state === "applied" && (
        <span className="flex flex-none items-center gap-1.5">
          <span className="text-[13px] leading-[18px] text-gray-900">
            {t("chat.action.applied")}
          </span>
          <Button variant="ghost" size="sm" onClick={onDismiss} disabled={busy}>
            {t("chat.action.undo")}
          </Button>
        </span>
      )}
      {action.state === "dismissed" && (
        <span className="flex-none text-[13px] leading-[18px] text-gray-900">
          {t("chat.action.dismissed")}
        </span>
      )}
    </div>
  );
};

export interface ChatTurnsProps {
  conversation: readonly SonaAgentChatTurnV1[];
  turn: AgentPanelTurnStatusV1 | null;
  proposal: AgentPanelProposalPreviewV1 | null;
  /** Wall clock, ticked by the owner so the elapsed numbers are testable. */
  now: number;
  /** This turn's optional evidence pack had at least one quoted source. */
  searchedCorpus: boolean;
  busy: boolean;
  onStop: () => void;
  onRetry: (message: string) => void;
  onApply: () => void;
  onUndo: () => void;
  onApplyAction: (actionIndex: number) => void;
  onDismissAction: (actionIndex: number) => void;
  onOpenLink: (link: string) => void;
}

/* A question of your own, as an object on the surface rather than a wash over
 * it. `bg-gray-alpha-200` was the hover tier — a question drawn in it read as
 * the row a pointer happened to be resting on — and on the frosted column an
 * ink alpha lets the backdrop show through the bubble. --surface-raised plus
 * the approved hairline is how the rest of the app says "this is an object",
 * and it is the same object in both materials. Flat: a bubble inside a 340pt
 * column is not floating, so it takes no shadow. */
const USER_BUBBLE =
  "max-w-[85%] self-end rounded-card border border-gray-alpha-400 bg-surface-raised px-3 py-2 text-[14px] leading-[21px] whitespace-pre-wrap text-gray-1000 [overflow-wrap:anywhere]";

/**
 * The scrollback: what was said, what the turn did on the way, the one card a
 * settings answer becomes, and the cards a corpus change is offered as.
 */
export const ChatTurns: React.FC<ChatTurnsProps> = ({
  conversation,
  turn,
  proposal,
  now,
  searchedCorpus,
  busy,
  onStop,
  onRetry,
  onApply,
  onUndo,
  onApplyAction,
  onDismissAction,
  onOpenLink,
}) => {
  const rows = conversationRows(conversation);
  const workIndex = workRowIndex(conversation, turn, searchedCorpus);
  const cardIndex = proposalRowIndex(conversation, proposal);
  const liveFailureIndex =
    turnFailure(turn) !== null && rows[rows.length - 1]?.turn.role === "user"
      ? rows.length - 1
      : -1;
  const liveFailureMessage =
    liveFailureIndex >= 0 ? rows[liveFailureIndex].turn.message : null;
  const work = turn !== null && workIndex >= 0 && (
    <TurnWork
      turn={turn}
      now={now}
      searchedCorpus={searchedCorpus}
      busy={busy}
      onStop={onStop}
      onRetry={onRetry}
      retryMessage={liveFailureMessage}
    />
  );

  return (
    <ul className="flex list-none flex-col gap-4 p-0">
      {rows.map(({ key, turn: row }, index) => (
        <React.Fragment key={key}>
          {index === workIndex && <li>{work}</li>}
          <li
            data-slot={row.role === "user" ? "chat-bubble" : "chat-answer"}
            className={row.role === "user" ? USER_BUBBLE : ""}
          >
            {row.role === "user" ? (
              row.message
            ) : index === cardIndex && proposal !== null ? (
              <ProposalCard
                proposal={proposal}
                busy={busy}
                onApply={onApply}
                onUndo={onUndo}
              />
            ) : (
              <AssistantText message={row.message} onOpenLink={onOpenLink} />
            )}
          </li>
          {row.outcome !== undefined &&
            row.outcome !== null &&
            index !== liveFailureIndex && (
              <li>
                <StoredOutcome
                  outcome={row.outcome}
                  busy={busy}
                  message={row.message}
                  onRetry={onRetry}
                />
              </li>
            )}
        </React.Fragment>
      ))}
      {/* A turn still working has no answer to sit above, so its work goes
          last — and a proposal with no row of its own is drawn rather than
          dropped. */}
      {workIndex === rows.length && <li>{work}</li>}
      {/* The offer sits under the answer that made it, because the answer is
          what explains it. Each card is its own row so a set of three reads
          as three choices rather than one block. */}
      {(turn?.actions ?? []).map((action) => (
        <li key={action.action_index}>
          <ChatActionCard
            action={action}
            busy={busy}
            onApply={() => onApplyAction(action.action_index)}
            onDismiss={() => onDismissAction(action.action_index)}
          />
        </li>
      ))}
      {cardIndex === -1 && proposal !== null && (
        <li>
          <ProposalCard
            proposal={proposal}
            busy={busy}
            onApply={onApply}
            onUndo={onUndo}
          />
        </li>
      )}
      {proposal?.rationale !== undefined && proposal.rationale !== "" && (
        <li>
          <AssistantText message={proposal.rationale} onOpenLink={onOpenLink} />
        </li>
      )}
      {proposal?.follow_up_question !== undefined &&
        proposal.follow_up_question !== null && (
          <li>
            <AssistantText
              message={proposal.follow_up_question}
              onOpenLink={onOpenLink}
            />
          </li>
        )}
    </ul>
  );
};

import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, MoreHorizontal, X } from "lucide-react";
import type {
  AgentChatConversationSummaryV1,
  AgentPanelCommandErrorV1,
  AgentPanelProposalPreviewV1,
  AgentPanelTurnStatusV1,
  AgentPanelWorkspaceV1,
  SonaAgentChatTurnV1,
} from "@/bindings";
import { Button } from "@/components/vg/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import { cn } from "@/lib/cn";
import { ChatComposer } from "./ChatComposer";
import { ChatHistorySubmenu } from "./ChatHistoryMenu";
import { ChatTurns } from "./ChatTurns";
import { CHAT_NOTICE_PHASES, isTurnRunning, sheetKeys } from "./chatModel";
import type { ChatPhase } from "./chatModel";

/* The column's width, and the one number the shell's arithmetic depends on.
 * The main window is a fixed 900pt and never resizes, so with the rail
 * collapsed to its 48pt glyph strip the page keeps 512pt beside the chat:
 * enough that the meeting you are asking about stays readable and stays
 * usable, which is the whole reason this is a column of the window rather
 * than a strip over the page. Border-box, so the hairline is inside the 340
 * rather than a 341st pixel taken off the page.
 *
 * Stated twice below on purpose, and only here: the outer box is the width the
 * layout gives up, the inner frame is the box that travels. */
const CHAT_WIDTH = "w-[340px]";

/* The frame only reads the shell's registered travel value. The shell root
 * owns that value's transition, so this frame never starts a second one: the
 * outer width box, rail and page resolve their final geometry in the press
 * frame, then this transform and the rail crossfade sample one timeline.
 *
 * `contain` keeps the moving subtree out of surrounding layout. `will-change`
 * is intentionally absent: styles/shell.css adds it through the moving gate
 * and removes it at the root's `transitionend`, rather than leaving a promoted
 * layer alive while chat is still. */
const CHAT_FRAME =
  "transition-none [transform:translateX(var(--shell-chat-offset))] [contain:layout_style]";

/* The line each command refusal shows, by the wire variant that raised it.
 *
 * Literal key names in a table rather than a key built from the variant: the
 * catalogue checker and a grep can both see these, and a variant added to the
 * backend fails this type until somebody has said what it tells the reader.
 *
 * Twelve of them share `chat.error.failed` because no catalogue has copy for
 * them yet. That request is filed; until it lands this is the sentence the
 * sheet already showed. The ten pairing reasons are read from where the
 * pairing panel keeps them, so one refusal is worded once. */
export const CHAT_ERROR_KEYS = {
  link_failed: "meetings.preview.linkFailed",
  unauthorized_window: "settings.agents.sonaAgent.reason.unauthorized_window",
  disabled: "settings.agents.sonaAgent.reason.disabled",
  unpaired: "settings.agents.sonaAgent.reason.unpaired",
  offline: "settings.agents.sonaAgent.reason.offline",
  invalid_configuration:
    "settings.agents.sonaAgent.reason.invalid_configuration",
  secret_unavailable: "settings.agents.sonaAgent.reason.secret_unavailable",
  untrusted_response: "settings.agents.sonaAgent.reason.untrusted_response",
  workspace_mismatch: "settings.agents.sonaAgent.reason.workspace_mismatch",
  remote_rejected: "settings.agents.sonaAgent.reason.remote_rejected",
  ownership_rejected: "settings.agents.sonaAgent.reason.ownership_rejected",
  unknown_conversation: "chat.error.failed",
  invalid_request: "chat.error.failed",
  turn_active: "chat.error.failed",
  unknown_turn: "chat.error.failed",
  unknown_proposal: "chat.error.failed",
  unknown_action: "chat.error.failed",
  action_failed: "chat.error.failed",
  confirmation_required: "chat.error.failed",
  stale_proposal: "chat.error.failed",
  invalid_proposal: "chat.error.failed",
  invalid_setting: "chat.error.failed",
  not_undoable: "chat.error.failed",
} satisfies Record<AgentPanelCommandErrorV1 | "link_failed", string>;

/* The two scopes, in the order the menu reads them. They are not two moods of
 * one brain: one answers questions from your own corpus, the other proposes
 * settings changes and can change nothing without a card and a click. Which
 * one a question goes to is the reader's to say, because a client-side guess
 * that misroutes sends a private question to the wrong sandbox, and there is
 * no honest heuristic for the difference between "what did I say about the
 * theme" and "change the theme". */
const SCOPES = ["sona_chat", "sona_config"] as const;

interface ChatSheetHeaderProps {
  history: readonly AgentChatConversationSummaryV1[];
  currentId: string | null;
  historyOpen: boolean;
  busy: boolean;
  workspace: AgentPanelWorkspaceV1;
  /** Where focus lands when the column opens with nothing typeable in it. */
  closeRef: React.RefObject<HTMLButtonElement | null>;
  onClose: () => void;
  onHistoryOpenChange: (open: boolean) => void;
  onSelectConversation: (conversationId: string) => void;
  onNewChat: () => void;
  onWorkspaceChange: (workspace: AgentPanelWorkspaceV1) => void;
}

/* The header is the title, the way out, and one menu.
 *
 * Everything a reader does to the conversation rather than in it — start a new
 * one, reopen an earlier one, choose which sandbox answers — hangs off that one
 * menu, because three glyphs on a 340pt header is three decisions offered to
 * somebody who came here to type a question. The two that survive on screen are
 * the two the column cannot be used without: its name and its exit.
 *
 * The chosen scope leaves the screen with the chip row, so the composer's
 * placeholder says which sandbox is listening instead. */
const ChatSheetHeader: React.FC<ChatSheetHeaderProps> = ({
  history,
  currentId,
  historyOpen,
  busy,
  workspace,
  closeRef,
  onClose,
  onHistoryOpenChange,
  onSelectConversation,
  onNewChat,
  onWorkspaceChange,
}) => {
  const { t } = useTranslation();
  return (
    /* Frosted, like the composer at the other end: the column reads as a
       quiet canvas between two bands of chrome, which is the reference
       panel's own structure. --surface-raised is that band under Solid;
       styles/shell.css swaps it for --glass-tint-dense under Glass. */
    <header
      data-slot="chat-header"
      className="flex flex-none items-center gap-1 border-b border-gray-alpha-400 bg-surface-raised py-2.5 ps-2 pe-2"
    >
      <Button
        ref={closeRef}
        variant="ghost"
        size="icon-sm"
        data-slot="chat-close"
        onClick={onClose}
        aria-label={t("chat.close")}
        title={t("chat.close")}
        className="text-gray-900"
      >
        <X aria-hidden="true" className="size-3.5" />
      </Button>
      <h2 className="min-w-0 flex-1 truncate text-[14px] leading-[21px] font-medium text-gray-1000">
        {t("chat.title")}
      </h2>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon-sm"
            data-slot="chat-more"
            aria-label={t("common.more")}
            className="text-gray-900"
          >
            <MoreHorizontal aria-hidden="true" className="size-3.5" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-60">
          <DropdownMenuItem
            data-slot="chat-new"
            disabled={busy}
            onSelect={onNewChat}
          >
            {t("chat.newChat")}
          </DropdownMenuItem>
          <ChatHistorySubmenu
            conversations={history}
            currentId={currentId}
            open={historyOpen}
            onOpenChange={onHistoryOpenChange}
            onSelect={onSelectConversation}
          />
          <DropdownMenuSeparator />
          <DropdownMenuLabel>{t("chat.scopeLabel")}</DropdownMenuLabel>
          <DropdownMenuRadioGroup
            value={workspace}
            // SAFETY: every radio item's value comes from SCOPES, which is the
            // workspace union itself, so the group can only report one of them.
            onValueChange={(next) =>
              onWorkspaceChange(next as AgentPanelWorkspaceV1)
            }
          >
            {SCOPES.map((scope) => (
              <DropdownMenuRadioItem key={scope} value={scope} disabled={busy}>
                {t(`chat.scope.${scope}`)}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>
    </header>
  );
};

interface ChatNoticeProps {
  phase: ChatPhase;
  onOpenSettings: () => void;
  onRetry: () => void;
}

/**
 * One line for each relay state that needs an explanation.
 *
 * A rate limit is temporary and the active turn is already retrying, so its
 * line offers no second action. Configuration failures still point to the one
 * Settings screen that owns the switch, pairing, address, and pinned key.
 * Offline and other errors also offer a fresh read.
 */
const ChatNotice: React.FC<ChatNoticeProps> = ({
  phase,
  onOpenSettings,
  onRetry,
}) => {
  const { t } = useTranslation();

  if (!(phase in CHAT_NOTICE_PHASES)) return null;

  return (
    <p
      data-slot="chat-notice"
      role="status"
      className="flex flex-none flex-wrap items-center gap-x-1.5 gap-y-1 border-t border-gray-alpha-400 px-3 py-2 text-[13px] leading-[18px] text-gray-900"
    >
      {t(`chat.status.${phase}`)}
      {(phase === "offline" || phase === "error") && (
        <Button variant="link" size="xs" onClick={onRetry}>
          {t("chat.retry")}
        </Button>
      )}
      {phase !== "rate_limited" && (
        <Button variant="link" size="xs" onClick={onOpenSettings}>
          {t("chat.openSettings")}
        </Button>
      )}
    </p>
  );
};

export interface ChatSheetProps {
  open: boolean;
  phase: ChatPhase;
  conversationId: string | null;
  conversation: readonly SonaAgentChatTurnV1[];
  turn: AgentPanelTurnStatusV1 | null;
  proposal: AgentPanelProposalPreviewV1 | null;
  history: readonly AgentChatConversationSummaryV1[];
  historyOpen: boolean;
  /** Wall clock, ticked by the owner so the elapsed numbers are testable. */
  now: number;
  draft: string;
  workspace: AgentPanelWorkspaceV1;
  /** The current Ask turn's pack contained at least one corpus source. */
  searchedCorpus: boolean;
  /**
   * The sheet is paired but the reader has not yet allowed matching quotes
   * to leave this Mac, so an Ask turn would go out without its evidence.
   */
  consentNeeded: boolean;
  /** A send, stop, apply, undo or history read is mid-flight. */
  busy: boolean;
  /** A typed command refusal, or a citation route the app could not open. */
  error: AgentPanelCommandErrorV1 | "link_failed" | null;
  onClose: () => void;
  onHistoryOpenChange: (open: boolean) => void;
  onSelectConversation: (conversationId: string) => void;
  onNewChat: () => void;
  onDraftChange: (draft: string) => void;
  onWorkspaceChange: (workspace: AgentPanelWorkspaceV1) => void;
  onSend: () => void;
  onStop: () => void;
  onApply: () => void;
  onUndo: () => void;
  onApplyAction: (actionIndex: number) => void;
  onDismissAction: (actionIndex: number) => void;
  onAllowRemote: () => void;
  onOpenLink: (link: string) => void;
  onOpenSettings: () => void;
  onRetry: () => void;
  onRetryTurn: (message: string) => void;
}

/**
 * The chat, as a column of the main window rather than a window of its own.
 *
 * It is mounted whether or not it is open. Opening gives its 340pt to the
 * layout in the press frame: the rail becomes 48, the page becomes 512, and
 * what the reader was reading stays beside the answer. The fixed inner frame
 * then slides into that already-reserved space on `transform`; closing reverses
 * the transform while the page gets its 680pt back immediately.
 *
 * A pure function of its props apart from one effect — where focus goes when
 * the column opens, which the pill cannot own because the pill has unmounted
 * by then. `ChatSheetHost` owns the IPC and the clock.
 */
export const ChatSheet: React.FC<ChatSheetProps> = ({
  open,
  phase,
  conversationId,
  conversation,
  turn,
  proposal,
  history,
  historyOpen,
  now,
  draft,
  workspace,
  consentNeeded,
  busy,
  error,
  searchedCorpus,
  onClose,
  onHistoryOpenChange,
  onSelectConversation,
  onNewChat,
  onDraftChange,
  onWorkspaceChange,
  onSend,
  onStop,
  onApply,
  onUndo,
  onApplyAction,
  onDismissAction,
  onAllowRemote,
  onOpenLink,
  onOpenSettings,
  onRetry,
  onRetryTurn,
}) => {
  const { t } = useTranslation();
  const errorText = error === null ? null : t(CHAT_ERROR_KEYS[error]);
  const running = isTurnRunning(turn);
  const columnRef = useRef<HTMLElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const fieldRef = useRef<HTMLTextAreaElement>(null);
  /** Whether this opening has already put the caret in the field. */
  const caretPlaced = useRef(false);
  /* Offline still takes a draft — the relay is the thing that is away, not the
   * column — and a proposal on screen does not stop a question. Loading is the
   * one beat where the field cannot take a keystroke, because the status it
   * would be sent under has not come back yet. */
  const composerDisabled = busy || phase === "loading" || phase === "disabled";

  /* Where focus goes when the column opens.
   *
   * It has to go somewhere: the press that opened it came from the rail's chat
   * row, which stays where it is. The field takes that focus, rather than
   * leaving it on a control outside the column that Escape cannot reach, or
   * falling back to the body — where Escape reaches nothing, because Escape is
   * bound on this element rather than on the window.
   *
   * The field is what the column is for, so the caret belongs in it. On the
   * first paint after a press it cannot take the caret — that is the loading
   * beat above — so the close button holds focus for it and the field takes
   * over the moment it wakes. Once placed, this stops moving anything; and it
   * moves nothing that a reader has since focused themselves, which is the
   * only other thing that can hold focus during that beat. */
  useEffect(() => {
    if (!open) {
      caretPlaced.current = false;
      return;
    }
    if (caretPlaced.current) return;
    const holder = document.activeElement;
    const openedFromRail =
      holder instanceof HTMLElement && holder.dataset.slot === "chat-rail-row";
    if (
      holder !== null &&
      holder !== document.body &&
      columnRef.current?.contains(holder) !== true &&
      !openedFromRail
    )
      return;
    const field = fieldRef.current;
    if (field === null || field.disabled) {
      closeRef.current?.focus();
      return;
    }
    field.focus();
    caretPlaced.current = true;
  }, [open, composerDisabled]);

  return (
    <aside
      ref={columnRef}
      data-slot="chat-sheet"
      aria-label={t("chat.title")}
      /* Closed it is out of the tab order and out of the accessibility tree:
         a column nobody can see must not be one a Tab key can walk into. */
      aria-hidden={open ? undefined : true}
      inert={!open}
      onKeyDown={sheetKeys(onClose)}
      /* The width, and nothing else. This box is what the layout gives up: it
         is 340 or it is 0, it changes in one frame, and it never animates —
         `transition-none` is stated rather than left to the initial `all`
         because "one moving part" is a promise, and a promise a stylesheet can
         contradict later is not one. The actual frame is fixed to the window
         below so its physical starting edge does not move when this width box
         changes. */
      className={cn(
        "relative z-20 min-h-0 flex-none transition-none",
        open ? CHAT_WIDTH : "pointer-events-none w-0",
      )}
    >
      {/* The frame, which is the column: the hairline that separates it from
          the page, the surface it is drawn on, and the only box in the window
          that moves. It is fixed to the window's trailing edge instead of the
          width box above: that box must snap from 340 to 0 to give the page
          its space back, while the frame has to stay at the same physical edge
          so a close can travel from its visible position to just beyond the
          window rather than jump there first. */}
      <div
        data-slot="chat-frame"
        className={cn(
          "fixed inset-y-0 end-0 z-20 flex min-h-0 flex-col border-s border-gray-alpha-500 bg-background-100",
          CHAT_WIDTH,
          CHAT_FRAME,
        )}
      >
        <ChatSheetHeader
          history={history}
          currentId={conversationId}
          historyOpen={historyOpen}
          busy={busy || running}
          closeRef={closeRef}
          onClose={onClose}
          onHistoryOpenChange={onHistoryOpenChange}
          onSelectConversation={onSelectConversation}
          onNewChat={onNewChat}
          workspace={workspace}
          onWorkspaceChange={onWorkspaceChange}
        />
        <div
          data-slot="chat-scroll"
          className="min-h-0 flex-1 overflow-y-auto px-3 pt-1 pb-4"
        >
          {phase === "loading" ? (
            <div className="flex h-full items-center justify-center">
              <Loader2
                aria-hidden="true"
                className="size-4 animate-spin text-gray-800 motion-reduce:animate-none"
              />
            </div>
          ) : conversation.length === 0 &&
            proposal === null &&
            turn === null ? (
            <div
              data-slot="chat-empty"
              className="flex h-full items-center justify-center"
            >
              <p className="max-w-[32ch] text-center text-[14px] leading-[21px] text-gray-900">
                {t("chat.empty")}
              </p>
            </div>
          ) : (
            <ChatTurns
              conversation={conversation}
              turn={turn}
              proposal={proposal}
              now={now}
              busy={busy}
              searchedCorpus={searchedCorpus}
              onStop={onStop}
              onRetry={onRetryTurn}
              onApply={onApply}
              onUndo={onUndo}
              onApplyAction={onApplyAction}
              onDismissAction={onDismissAction}
              onOpenLink={onOpenLink}
            />
          )}
        </div>
        <ChatNotice
          phase={phase}
          onOpenSettings={onOpenSettings}
          onRetry={onRetry}
        />
        {/* The consent gate, surfaced where it bites. Without it an Ask turn
            goes out with no pack, no card and no tools, and the model says it
            was not given Sona's records; the reader would go looking for a
            switch in Settings that this row can throw for them. */}
        {consentNeeded && phase === "ready" && (
          <p
            data-slot="chat-consent"
            role="status"
            className="flex flex-none flex-wrap items-center gap-x-1.5 gap-y-1 border-t border-gray-alpha-400 px-3 py-2 text-[13px] leading-[18px] text-gray-900"
          >
            {t("chat.consent.notice")}
            <Button
              variant="link"
              size="xs"
              disabled={busy}
              onClick={onAllowRemote}
            >
              {t("chat.consent.allow")}
            </Button>
          </p>
        )}
        {errorText !== null && (
          <p
            role="alert"
            className="flex-none border-t border-gray-alpha-400 px-3 py-2 text-[13px] leading-[18px] text-red-900 [overflow-wrap:anywhere]"
          >
            {errorText}
          </p>
        )}
        <ChatComposer
          workspace={workspace}
          draft={draft}
          fieldRef={fieldRef}
          running={running}
          disabled={composerDisabled}
          onDraftChange={onDraftChange}
          onSend={onSend}
          onStop={onStop}
        />
      </div>
    </aside>
  );
};

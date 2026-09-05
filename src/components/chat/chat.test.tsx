import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import { AppContent } from "@/App";
import { commands } from "@/bindings";
import type {
  AgentChatConversationSummaryV1,
  AgentPanelActionV1,
  AgentPanelCommandErrorV1,
  AgentPanelProposalPreviewV1,
  AgentPanelStatusV1,
  AgentPanelTurnStatusV1,
  AgentPanelWorkspaceV1,
  SonaAgentChatTurnV1,
} from "@/bindings";
import { askSona } from "@/components/commandPaletteSearch";
import { CHAT_ERROR_KEYS, ChatSheet } from "./ChatSheet";
import { sendSheetTurn } from "./ChatSheetHost";
import type { ChatPhase } from "./chatModel";
import {
  chatPhase,
  composerKeys,
  isStillWaiting,
  linkifySona,
  needsRemoteConsent,
  proposalRowIndex,
  retryMessage,
  sheetKeys,
  stepMs,
  turnFailure,
  workedMs,
  workRowIndex,
} from "./chatModel";

/* The chat sheet: the six states it can be in, the two gestures that open and
 * close it, and the two callers that now target it instead of a second window.
 *
 * Copy comes from the shipped en bundle rather than a fixture, so a missing
 * `chat.*` key fails here as a raw key in the markup instead of on a screen.
 * `renderToStaticMarkup` runs no effects and no events, which is why the
 * gestures are pinned against the exported handlers. */

const localeFile = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

// SAFETY: the en bundle is repo-owned and the catalogue tests pin these keys;
// the narrow states the shape this test reads, not a guess about foreign data.
const en = JSON.parse(fs.readFileSync(localeFile, "utf8")) as {
  common: {
    open: string;
    more: string;
  };
  settings: {
    agents: {
      sonaAgent: {
        reason: Record<"offline", string>;
      };
    };
  };
  meetings: {
    preview: {
      linkFailed: string;
    };
  };
  chat: {
    empty: string;
    title: string;
    close: string;
    newChat: string;
    placeholder: string;
    placeholderConfig: string;
    send: string;
    stop: string;
    retry: string;
    openSettings: string;
    workedFor: string;
    error: Record<
      "unreachable" | "refused" | "failed" | "too_many_lookups",
      string
    >;
    working: Record<"searchedCorpus" | "stillWaiting" | "cancel", string>;
    status: Record<"disabled" | "unpaired" | "offline" | "error", string>;
    turnState: Record<"running", string>;
    proposal: Record<"apply" | "applied" | "undo", string>;
    action: Record<
      | "resolve_loop"
      | "add_vocabulary_term"
      | "apply"
      | "dismiss"
      | "applied"
      | "undo"
      | "dismissed",
      string
    >;
    consent: Record<"notice" | "allow", string>;
    tool: Record<"word_stats" | "search", string>;
  };
};

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localeFile, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

const paint = (node: React.ReactElement): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const occurrences = (markup: string, needle: string): number =>
  markup.split(needle).length - 1;

/* `renderToStaticMarkup` escapes text, so an expectation lifted from the JSON
 * bundle has to be escaped the same way before it can be looked for. */
const escaped = (text: string): string =>
  text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#x27;");

const noop = () => undefined;

/* The sentence a dotted key names in the en bundle. i18next answers a key it
 * cannot find with the key itself, so a lookup that fails here is what would
 * have reached the screen. */
const sentence = (key: string): string => {
  if (!i18n.exists(key)) throw new Error(`no en copy for ${key}`);
  return i18n.t(key);
};

const TURN: AgentPanelTurnStatusV1 = {
  turn_id: "t1",
  workspace: "sona_chat",
  state: "running",
  event_cursor: 0,
  started_at_utc_ms: 1_000,
  completed_at_utc_ms: null,
  steps: [],
  actions: [],
  failure: null,
};

const PROPOSAL: AgentPanelProposalPreviewV1 = {
  proposal_id: "p1",
  summary: "Disable filler-word removal for Email mode.",
  rationale: "You asked for verbatim transcripts in that mode.",
  actions: [{ key: "theme", value: "dark" }],
  follow_up_question: null,
  source_settings_revision: 7,
  confirmation: "automatic",
  state: "pending",
  receipt_id: null,
  applied_revision: null,
};

const user = (message: string): SonaAgentChatTurnV1 => ({
  role: "user",
  message,
});
const assistant = (message: string): SonaAgentChatTurnV1 => ({
  role: "assistant",
  message,
});

interface SheetCase {
  open?: boolean;
  phase?: ChatPhase;
  conversation?: readonly SonaAgentChatTurnV1[];
  turn?: AgentPanelTurnStatusV1 | null;
  proposal?: AgentPanelProposalPreviewV1 | null;
  history?: readonly AgentChatConversationSummaryV1[];
  historyOpen?: boolean;
  now?: number;
  searchedCorpus?: boolean;
  consentNeeded?: boolean;
  workspace?: AgentPanelWorkspaceV1;
  error?: AgentPanelCommandErrorV1 | "link_failed" | null;
}
const sheet = ({
  open = true,
  phase = "ready",
  conversation = [],
  turn = null,
  proposal = null,
  history = [],
  historyOpen = false,
  now = 6_000,
  searchedCorpus = false,
  consentNeeded = false,
  workspace = "sona_chat",
  error = null,
}: SheetCase = {}): string =>
  paint(
    <ChatSheet
      open={open}
      phase={phase}
      conversationId={null}
      conversation={conversation}
      turn={turn}
      searchedCorpus={searchedCorpus}
      proposal={proposal}
      history={history}
      historyOpen={historyOpen}
      now={now}
      draft=""
      workspace={workspace}
      consentNeeded={consentNeeded}
      busy={false}
      error={error}
      onClose={noop}
      onHistoryOpenChange={noop}
      onSelectConversation={noop}
      onNewChat={noop}
      onDraftChange={noop}
      onWorkspaceChange={noop}
      onSend={noop}
      onStop={noop}
      onApply={noop}
      onUndo={noop}
      onApplyAction={noop}
      onDismissAction={noop}
      onAllowRemote={noop}
      onOpenLink={noop}
      onOpenSettings={noop}
      onRetry={noop}
      onRetryTurn={noop}
    />,
  );

describe("the column's shape", () => {
  /* Closed it is still mounted — the shell needs a 0pt structural column to
   * return the page to 680 — so nothing can reach or read its fixed frame. */
  test("closed: mounted, 0 wide, inert, and out of the a11y tree", () => {
    const markup = sheet({ open: false });

    expect(markup).toContain('data-slot="chat-sheet"');
    expect(markup).toContain('data-slot="chat-frame"');
    expect(markup).toContain('aria-hidden="true"');
    expect(markup).toContain("inert");
    expect(markup).toContain("pointer-events-none w-0");
  });

  /* The layout takes the 340 in the press frame. The fixed frame reads the
   * root shell's registered timeline after that; it owns no transition of its
   * own, and no scrim or blur returns the page to being hidden behind chat. */
  test("open: 340 of layout and a fixed frame on the shell timeline", () => {
    const markup = sheet();

    expect(markup).toContain('data-slot="chat-frame"');
    expect(markup).toContain("transition-none");
    expect(markup).toContain(
      "[transform:translateX(var(--shell-chat-offset))]",
    );
    expect(markup).toContain("w-[340px]");
    // One stronger hairline against the page, and no dimming of what is behind it.
    expect(markup).toContain("border-s border-gray-alpha-500");
    expect(markup).not.toContain("bg-black/");
    expect(markup).not.toContain("backdrop-blur");
  });

  /* Two boxes: the structural width box that makes room for the page, and the
   * fixed frame that keeps one physical window edge through both the open and
   * closed geometry. That stable edge is what lets a close slide out rather
   * than jump to the edge before it moves. */
  test("open: a structural width box and one fixed contained frame", () => {
    const markup = sheet();
    const outer = /<aside[^>]*class="([^"]*)"/.exec(markup)?.[1] ?? "";

    expect(outer).toContain("transition-none");
    expect(outer).toContain("flex-none");
    expect(outer).toContain("w-[340px]");
    expect(outer).not.toContain("border-s");
    expect(markup).toContain("fixed inset-y-0 end-0");
    expect(markup).toContain("[contain:layout_style]");
    // Stated on both boxes, and nowhere else.
    expect(occurrences(markup, "w-[340px]")).toBe(2);
  });

  /* Two glyphs and a name. Every other verb the column carries — a new chat,
   * an earlier one, which sandbox answers — is inside the one menu, and the
   * menu's own contents are portalled, so what is checkable here is that the
   * header offers exactly two presses. */
  test("the header is the title, the way out, and one menu", () => {
    const markup = sheet();
    const header = markup.slice(
      markup.indexOf('data-slot="chat-header"'),
      markup.indexOf("</header>"),
    );

    expect(occurrences(header, "<button")).toBe(2);
    expect(occurrences(header, 'data-slot="chat-close"')).toBe(1);
    expect(occurrences(header, 'data-slot="chat-more"')).toBe(1);
    expect(header).toContain(`aria-label="${en.chat.close}"`);
    expect(header).toContain(`aria-label="${en.common.more}"`);
    expect(header).toContain(`<h2`);
    expect(header).toContain(en.chat.title);
    expect(markup).toContain("border-b border-gray-alpha-400");
  });

  test("empty: one invitation, and a composer that is a field and a glyph", () => {
    const markup = sheet();

    expect(markup).toContain(escaped(en.chat.empty));
    expect(markup).toContain(`placeholder="${en.chat.placeholder}"`);
    expect(markup).toContain(`aria-label="${en.chat.send}"`);
    expect(markup).not.toContain('data-slot="chat-stop"');
    // The scope chips went into the header's menu; the band is the question.
    expect(markup).not.toContain('role="radio"');
  });

  /* The chosen sandbox is no longer a chip on screen, so the field is what
   * says it: a reader mid-sentence in Configure would otherwise have nothing
   * telling them this question proposes a settings change. */
  test("each scope names itself in the field", () => {
    const cases = [
      ["sona_chat", "placeholder"],
      ["sona_config", "placeholderConfig"],
    ] as const;

    for (const [workspace, key] of cases)
      expect(sheet({ phase: "unpaired", workspace })).toContain(
        `placeholder="${en.chat[key]}"`,
      );
  });

  /* Settings is a destination, Configure is a sandbox; one column shows both
   * words, so they may not be the same word. */
  test("the Configure scope is distinct from the Settings destination", () => {
    expect(en.chat.placeholderConfig).not.toBe(en.chat.openSettings);
    expect(sheet({ phase: "unpaired", workspace: "sona_config" })).toContain(
      en.chat.openSettings,
    );
  });

  /* One sentence for both scopes. The empty state is keyed on an absent
   * conversation and nothing else, so a sentence that is only true of Ask is
   * a sentence that lies to whoever has Configure selected. */
  test("empty: the same invitation under either scope", () => {
    for (const workspace of ["sona_chat", "sona_config"] as const)
      expect(sheet({ workspace })).toContain(escaped(en.chat.empty));
  });
});

describe("a turn on screen", () => {
  test("running: a live line, and the send glyph becomes a stop", () => {
    const markup = sheet({
      conversation: [user("What did we decide?")],
      turn: TURN,
    });

    expect(markup).toContain('data-slot="chat-work"');
    expect(markup).toContain(en.chat.turnState.running);
    // 6000 - 1000, whole seconds.
    expect(markup).toContain("5s");
    expect(markup).toContain('data-slot="chat-stop"');
    expect(markup).not.toContain('data-slot="chat-send"');
  });

  test("a live turn stays visible while its conversation is still arriving", () => {
    const markup = sheet({ turn: TURN });

    expect(markup).toContain('data-slot="chat-work"');
    expect(markup).not.toContain('data-slot="chat-empty"');
  });

  test("failed: one typed failure line and a retry under its question", () => {
    for (const failure of ["unreachable", "refused", "failed"] as const) {
      const markup = sheet({
        conversation: [user("What did we decide?")],
        turn: {
          ...TURN,
          state: "failed",
          completed_at_utc_ms: 4_000,
          failure,
        },
      });

      expect(markup).toContain('data-slot="chat-turn-error"');
      expect(markup).toContain(escaped(en.chat.error[failure]));
      expect(markup).toContain(en.chat.retry);
      expect(markup).not.toContain('data-slot="chat-stop"');
    }
  });

  test("waiting: offers the existing cancel action after thirty seconds", () => {
    const queued = { ...TURN, state: "queued" as const };

    expect(isStillWaiting(queued, 30_999)).toBe(false);
    expect(isStillWaiting(queued, 31_000)).toBe(true);
    expect(isStillWaiting({ ...queued, state: "waiting_user" }, 31_000)).toBe(
      false,
    );

    const markup = sheet({
      conversation: [user("What did we decide?")],
      turn: queued,
      now: 31_000,
    });

    expect(markup).toContain('data-slot="chat-still-waiting"');
    expect(markup).toContain(en.chat.working.stillWaiting);
    expect(markup).toContain(en.chat.working.cancel);
  });

  test("marks the sheet turn only when its pack had sources", () => {
    const turn = {
      ...TURN,
      state: "succeeded" as const,
      completed_at_utc_ms: 4_000,
    };
    const sourced = sheet({
      conversation: [user("What did we decide?"), assistant("We decided.")],
      turn,
      searchedCorpus: true,
    });
    const packless = sheet({
      conversation: [user("What did we decide?"), assistant("We decided.")],
      turn,
    });

    expect(sourced).toContain('data-slot="chat-searched-corpus"');
    expect(sourced).toContain(escaped(en.chat.working.searchedCorpus));
    expect(packless).not.toContain('data-slot="chat-searched-corpus"');
  });

  /* Steps exist: the line becomes a disclosure, collapsed, with one row and a
   * duration per step.
   *
   * The disclosure is a button with `aria-expanded` and an `aria-controls`
   * list, not <details>/<summary>. WebKit's accessibility layer exposes no
   * press action on a <summary>, so a live accessibility run could not open
   * this list at all, and the app ships in a WKWebView. The list is `hidden`
   * while closed rather than unmounted, which is what keeps it out of the
   * accessibility tree and the tab order while leaving `aria-controls`
   * pointing at a node that exists — and is why the step's own label is in
   * the closed markup below. */
  test("running with steps: the line folds the steps away, closed", () => {
    const markup = sheet({
      conversation: [user("What did we decide?")],
      turn: {
        ...TURN,
        steps: [
          {
            id: "s1",
            label: "Read the transcript",
            state: "done",
            started_after_ms: 500,
            ended_after_ms: 2_500,
            tool: null,
          },
        ],
      },
    });

    expect(markup).not.toContain("<details");
    expect(markup).toContain('data-slot="chat-steps-toggle"');
    expect(markup).toContain('aria-expanded="false"');
    /* The button names the list it opens, and that list is the one on the
       page: a dangling `aria-controls` is a disclosure a screen reader cannot
       follow. */
    const controls = /aria-controls="([^"]+)"/.exec(markup)?.[1] ?? "";
    expect(controls).not.toBe("");
    /* `hidden` on that same element, not merely somewhere in the markup:
       `aria-hidden="true"` also contains the substring, so the two attributes
       are matched together or the assertion proves nothing. */
    expect(markup).toContain(`id="${controls}" hidden=""`);
    expect(markup).toContain("Read the transcript");
    // 2500 - 500.
    expect(markup).toContain("2s");
  });

  /* The tool steps' own reading. A tool's name is a machine's name, so it
   * renders as a mono uppercase chip rather than as a sentence beside the
   * model's own prose steps — those two are not the same kind of event, and
   * they used to be typeset identically. */
  test("a tool step wears the chip; a thought step stays prose", () => {
    const markup = sheet({
      conversation: [user("How many words?"), assistant("4,812.")],
      turn: {
        ...TURN,
        state: "succeeded",
        completed_at_utc_ms: 4_000,
        steps: [
          {
            id: "tool-1-0",
            label: "word_stats",
            state: "done",
            started_after_ms: 1_000,
            ended_after_ms: 2_000,
            tool: "word_stats",
          },
          {
            id: "step-1",
            label: "Thought about it",
            state: "done",
            started_after_ms: 2_000,
            ended_after_ms: 3_000,
            tool: null,
          },
        ],
      },
    });

    const chip = new RegExp(
      `<span data-slot="chat-tool" class="([^"]*)">${en.chat.tool.word_stats}`,
    ).exec(markup);
    expect(chip).not.toBeNull();
    expect(chip?.[1]).toContain("font-mono");
    expect(chip?.[1]).toContain("uppercase");
    expect(chip?.[1]).toContain("tracking-[0.08em]");
    /* Hairline, no fill: a chip that fills reads as a button. */
    expect(chip?.[1]).toContain("border-gray-alpha-400");
    expect(chip?.[1]).not.toContain("bg-");
    /* The raw tool key never reaches the screen; the localized name does. */
    expect(markup).not.toContain(">word_stats<");
    /* And a step the model narrated is prose, chipless. */
    expect(markup).toContain("Thought about it");
    expect(occurrences(markup, 'data-slot="chat-tool"')).toBe(1);
  });

  test("answered: the reply is prose with a labeled source link and elapsed time", () => {
    const markup = sheet({
      conversation: [
        user("Where did we say that?"),
        assistant("In sona://dictation/75, before the break."),
      ],
      turn: { ...TURN, state: "succeeded", completed_at_utc_ms: 4_000 },
    });

    expect(markup).toContain('data-slot="chat-citation"');
    expect(markup).toContain('aria-label="sona://dictation/75"');
    expect(markup).toContain(">sona://dictation/75</button>");
    expect(markup).toContain(
      escaped(en.chat.workedFor.replace("{{seconds}}", "3")),
    );
    expect(markup).not.toContain('data-slot="chat-stop"');
    expect(markup).toContain('data-slot="chat-work"');
  });

  test("completed without a recorded finish does not invent elapsed time", () => {
    const markup = sheet({
      conversation: [user("q"), assistant("a")],
      turn: { ...TURN, state: "succeeded", completed_at_utc_ms: null },
    });

    expect(markup).not.toContain('data-slot="chat-work"');
  });

  /* With steps there is a line, and its number is fixed by the backend — so
   * reopening the sheet tomorrow still says how long it took rather than how
   * long ago it was. */
  test("answered with steps: the disclosure freezes at the turn's own length", () => {
    const markup = sheet({
      now: 900_000,
      conversation: [user("q"), assistant("a")],
      turn: {
        ...TURN,
        state: "succeeded",
        completed_at_utc_ms: 4_000,
        steps: [
          {
            id: "s1",
            label: "Read the transcript",
            state: "done",
            started_after_ms: 0,
            ended_after_ms: 3_000,
            tool: null,
          },
        ],
      },
    });

    expect(markup).toContain('data-slot="chat-work"');
    expect(markup).toContain(
      escaped(en.chat.workedFor.replace("{{seconds}}", "3")),
    );
  });
});

describe("a settings answer", () => {
  test("pending: one card carrying the summary, the change set and Apply", () => {
    const markup = sheet({
      conversation: [user("Go dark"), assistant(PROPOSAL.summary)],
      proposal: PROPOSAL,
    });

    expect(occurrences(markup, 'data-slot="chat-proposal"')).toBe(1);
    // The summary is the card's title and appears once, not once as a message
    // and once as a card.
    expect(occurrences(markup, PROPOSAL.summary)).toBe(1);
    expect(markup).toContain("theme");
    expect(markup).toContain(en.chat.proposal.apply);
    expect(markup).not.toContain(en.chat.proposal.undo);
    /* And it is readable in full at the column's 340: this sentence is the
     * whole of what the assistant said — the card took the row it would
     * otherwise have been printed in — so a truncated one would be an Apply
     * under half a sentence. */
    const summary =
      new RegExp(`<span class="([^"]*)">${PROPOSAL.summary}`).exec(
        markup,
      )?.[1] ?? "";
    expect(summary).not.toContain("truncate");
    expect(summary).toContain("[overflow-wrap:anywhere]");
  });

  test("applied: the same card, now saying so, with an undo beside it", () => {
    const markup = sheet({
      conversation: [user("Go dark"), assistant(PROPOSAL.summary)],
      proposal: {
        ...PROPOSAL,
        state: "applied",
        receipt_id: "receipt-p1-8",
        applied_revision: 8,
      },
    });

    expect(occurrences(markup, 'data-slot="chat-proposal"')).toBe(1);
    expect(markup).toContain(en.chat.proposal.applied);
    expect(markup).toContain(en.chat.proposal.undo);
    expect(markup).not.toContain(`>${en.chat.proposal.apply}<`);
  });
});

const RESOLVE: AgentPanelActionV1 = {
  action_index: 0,
  action: {
    kind: "resolve_loop",
    reason: "You said in the meeting that the deck went out.",
    loop_id: "m-1:commitment:0123456789abcdef",
  },
  state: "pending",
  operation_id: null,
};

const offering = (
  ...actions: AgentPanelActionV1[]
): AgentPanelTurnStatusV1 => ({
  ...TURN,
  state: "succeeded",
  completed_at_utc_ms: 4_000,
  actions,
});

describe("a corpus change the answer offered", () => {
  test("pending: what changes, why, and both ways to answer it", () => {
    const markup = sheet({
      conversation: [user("Close the deck commitment"), assistant("Done?")],
      turn: offering(RESOLVE),
    });

    expect(occurrences(markup, 'data-slot="chat-action"')).toBe(1);
    expect(markup).toContain(en.chat.action.resolve_loop);
    expect(markup).toContain(escaped(RESOLVE.action.reason));
    expect(markup).toContain(en.chat.action.apply);
    expect(markup).toContain(en.chat.action.dismiss);
    expect(markup).not.toContain(en.chat.action.applied);
    /* A loop id is a digest. The card names the kind of change and leaves the
     * row to the reason, which is a sentence about that one commitment. */
    expect(markup).not.toContain("m-1:commitment:0123456789abcdef");
  });

  test("applied: the same card, now saying so, with an undo beside it", () => {
    const markup = sheet({
      turn: offering({
        ...RESOLVE,
        state: "applied",
        operation_id: "3f1a-op",
      }),
    });

    expect(occurrences(markup, 'data-slot="chat-action"')).toBe(1);
    expect(markup).toContain(en.chat.action.applied);
    expect(markup).toContain(en.chat.action.undo);
    expect(markup).not.toContain(`>${en.chat.action.apply}<`);
  });

  test("dismissed: a card that says so and offers nothing", () => {
    const markup = sheet({
      turn: offering({ ...RESOLVE, state: "dismissed" }),
    });

    expect(markup).toContain(en.chat.action.dismissed);
    expect(markup).not.toContain(`>${en.chat.action.apply}<`);
    expect(markup).not.toContain(`>${en.chat.action.undo}<`);
  });

  /* Three offers are three choices, each answerable on its own. */
  test("a set of offers is a card each, in the order they were offered", () => {
    const markup = sheet({
      turn: offering(RESOLVE, {
        action_index: 1,
        action: {
          kind: "add_vocabulary_term",
          reason: "Sona keeps writing it as two words.",
          term: "north star",
          replacement: "Northstar",
        },
        state: "pending",
        operation_id: null,
      }),
    });

    expect(occurrences(markup, 'data-slot="chat-action"')).toBe(2);
    expect(markup.indexOf(en.chat.action.resolve_loop)).toBeLessThan(
      markup.indexOf("Northstar"),
    );
  });

  test("an answer with nothing to offer draws no card", () => {
    const markup = sheet({
      conversation: [user("What did we decide?"), assistant("The deck ships.")],
      turn: offering(),
    });

    expect(markup).not.toContain('data-slot="chat-action"');
  });
});

describe("the consent notice", () => {
  test("a paired sheet without consent says what would leave, with the switch", () => {
    const markup = sheet({ consentNeeded: true });

    expect(markup).toContain('data-slot="chat-consent"');
    expect(markup).toContain(escaped(en.chat.consent.notice));
    expect(markup).toContain(en.chat.consent.allow);
  });

  test("absent once consent is given, and never over another phase's notice", () => {
    expect(sheet()).not.toContain('data-slot="chat-consent"');
    expect(sheet({ consentNeeded: true, phase: "unpaired" })).not.toContain(
      'data-slot="chat-consent"',
    );
  });

  test("the predicate is the pack gate's complement on a paired Ask sheet", () => {
    expect(
      needsRemoteConsent("sona_chat", {
        paired: true,
        remoteIntelligence: false,
      }),
    ).toBe(true);
    expect(
      needsRemoteConsent("sona_chat", {
        paired: true,
        remoteIntelligence: true,
      }),
    ).toBe(false);
    expect(
      needsRemoteConsent("sona_chat", {
        paired: false,
        remoteIntelligence: false,
      }),
    ).toBe(false);
    expect(
      needsRemoteConsent("sona_config", {
        paired: true,
        remoteIntelligence: false,
      }),
    ).toBe(false);
  });
});

describe("a lookup the Mac ran for the model", () => {
  /* The panel names the tool; the sheet says it in the reader's language. A
   * step the relay reported carries no tool and shows its own label. */
  test("a tool step is labelled by its tool, a relay step by its label", () => {
    const markup = sheet({
      conversation: [user("What are my most used words?")],
      turn: {
        ...TURN,
        steps: [
          {
            id: "tool-1-0",
            label: "word_stats",
            state: "done",
            started_after_ms: 1_000,
            ended_after_ms: 2_000,
            tool: "word_stats",
          },
          {
            id: "step-1",
            label: "Thought about it",
            state: "done",
            started_after_ms: 2_000,
            ended_after_ms: 3_000,
            tool: null,
          },
        ],
      },
    });

    expect(markup).toContain(en.chat.tool.word_stats);
    expect(markup).not.toContain(">word_stats<");
    expect(markup).toContain("Thought about it");
  });

  test("a fourth round of lookups ends the turn with its own sentence", () => {
    const markup = sheet({
      conversation: [user("What are my most used words?")],
      turn: {
        ...TURN,
        state: "failed",
        completed_at_utc_ms: 9_000,
        failure: "too_many_lookups",
      },
    });

    expect(markup).toContain(escaped(en.chat.error.too_many_lookups));
  });
});

describe("command feedback", () => {
  test("an unroutable citation says it could not open instead of exposing its route", () => {
    const markup = sheet({ error: "link_failed" });

    expect(markup).toContain(escaped(en.meetings.preview.linkFailed));
    expect(markup).not.toContain(">link_failed<");
  });

  test("typed command reasons are localized without rendering their codes", () => {
    const relay = sheet({ error: "offline" });
    const local = sheet({ error: "unknown_action" });

    expect(relay).toContain(
      escaped(en.settings.agents.sonaAgent.reason.offline),
    );
    expect(relay).not.toContain(">offline<");
    expect(local).toContain(escaped(en.chat.error.failed));
    expect(local).not.toContain(">unknown_action<");
  });

  /* The two above are refusals with copy of their own. This is the rest of
   * the table: every refusal the backend can raise reaches the reader as a
   * sentence, and none arrives as the key that was meant to name one. */
  test("every command refusal renders catalogue copy, not a key or a code", () => {
    for (const [error, key] of Object.entries(CHAT_ERROR_KEYS)) {
      // SAFETY: CHAT_ERROR_KEYS satisfies Record over exactly this union, so
      // every key Object.entries yields is one of its members.
      const markup = sheet({
        error: error as AgentPanelCommandErrorV1 | "link_failed",
      });

      expect(markup).toContain(escaped(sentence(key)));
      expect(markup).not.toContain(key);
      expect(markup).not.toContain(`>${error}<`);
    }
  });
});

describe("the states where nothing would answer", () => {
  test("unpaired: one line and a way to Settings, conversation intact", () => {
    const markup = sheet({
      phase: "unpaired",
      conversation: [user("Anything?")],
    });

    expect(markup).toContain(escaped(en.chat.status.unpaired));
    expect(markup).toContain(en.chat.openSettings);
    // The scrollback is not blanked by a relay that went away.
    expect(markup).toContain("Anything?");
  });

  test("each broken phase says its own sentence, once", () => {
    for (const phase of ["disabled", "offline", "error"] as const) {
      const markup = sheet({ phase });
      expect(occurrences(markup, escaped(en.chat.status[phase]))).toBe(1);
    }
  });

  /* All four are repaired on one screen — the switch, the pairing, the address
   * and the pinned key — so all four have to be able to reach it. A retry is
   * offered beside it only where a re-read could plausibly change the answer.
   *
   * Read the notice rather than the whole sheet so this assertion only covers
   * the recovery action. */
  test("every broken phase offers Settings, and only two offer a retry", () => {
    const notice = (markup: string): string => {
      const start = markup.indexOf('data-slot="chat-notice"');
      expect(start).toBeGreaterThan(-1);
      return markup.slice(start, markup.indexOf("</p>", start));
    };

    for (const phase of ["disabled", "unpaired", "offline", "error"] as const) {
      const line = notice(sheet({ phase }));
      expect(occurrences(line, en.chat.openSettings)).toBe(1);
      expect(occurrences(line, en.chat.retry)).toBe(
        phase === "offline" || phase === "error" ? 1 : 0,
      );
    }
  });

  test("ready says nothing about the relay at all", () => {
    expect(sheet()).not.toContain('data-slot="chat-notice"');
  });
});

describe("the model behind the sheet", () => {
  test("ten relay statuses collapse onto the six the sheet acts on", () => {
    const status = (
      relay: AgentPanelStatusV1["relay_status"],
    ): AgentPanelStatusV1 => ({
      invalidation_id: 1,
      relay_status: relay,
      conversation_id: null,
      conversation: [],
      turn: null,
      proposal: null,
    });

    expect(chatPhase(null)).toBe("loading");
    expect(chatPhase(status("ready"))).toBe("ready");
    expect(chatPhase(status("disabled"))).toBe("disabled");
    expect(chatPhase(status("unpaired"))).toBe("unpaired");
    expect(chatPhase(status("offline"))).toBe("offline");
    for (const relay of [
      "invalid_configuration",
      "secret_unavailable",
      "untrusted_response",
      "workspace_mismatch",
      "remote_rejected",
      "ownership_rejected",
    ] as const) {
      expect(chatPhase(status(relay))).toBe("error");
    }
  });

  test("the work row sits above the answer, and after a live question", () => {
    const finished = {
      ...TURN,
      state: "succeeded" as const,
      completed_at_utc_ms: 4_000,
      steps: [
        {
          id: "s1",
          label: "Read",
          state: "done" as const,
          started_after_ms: 0,
          ended_after_ms: 1,
          tool: null,
        },
      ],
    };

    expect(workRowIndex([user("q"), assistant("a")], finished)).toBe(1);
    expect(workRowIndex([user("q")], TURN)).toBe(1);
    expect(
      workRowIndex([user("q"), assistant("a")], {
        ...TURN,
        state: "succeeded",
        completed_at_utc_ms: 4_000,
      }),
    ).toBe(1);
    expect(
      workRowIndex([user("q"), assistant("a")], {
        ...TURN,
        state: "succeeded",
      }),
    ).toBe(-1);
  });

  test("a typed failure owns the retry question and its work row", () => {
    const failed = {
      ...TURN,
      state: "failed" as const,
      completed_at_utc_ms: 4_000,
      failure: "unreachable" as const,
    };
    const conversation = [
      user("Earlier question"),
      assistant("Earlier answer"),
      user("Retry this question"),
    ];

    expect(turnFailure(failed)).toBe("unreachable");
    expect(retryMessage(conversation, failed)).toBe("Retry this question");
    expect(retryMessage(conversation, TURN)).toBeNull();
    expect(workRowIndex(conversation, failed)).toBe(3);
  });

  test("the proposal takes the row whose words it already is", () => {
    expect(
      proposalRowIndex([user("q"), assistant(PROPOSAL.summary)], PROPOSAL),
    ).toBe(1);
    expect(proposalRowIndex([user("q"), assistant("other")], PROPOSAL)).toBe(
      -1,
    );
    expect(proposalRowIndex([user("q")], null)).toBe(-1);
  });

  test("a finished turn's elapsed time stops moving", () => {
    const finished = { ...TURN, completed_at_utc_ms: 4_000 };

    expect(workedMs(finished, 6_000)).toBe(3_000);
    expect(workedMs(finished, 900_000)).toBe(3_000);
    expect(workedMs(TURN, 6_000)).toBe(5_000);
  });

  test("a running step is measured against the clock, a finished one is not", () => {
    const running = {
      id: "s",
      label: "Read",
      state: "running" as const,
      started_after_ms: 1_000,
      ended_after_ms: null,
      tool: null,
    };

    expect(stepMs(running, TURN, 6_000)).toBe(4_000);
    expect(stepMs({ ...running, ended_after_ms: 2_000 }, TURN, 6_000)).toBe(
      1_000,
    );
  });

  test("sona addresses split out of prose, sentence punctuation excluded", () => {
    expect(linkifySona("See sona://meeting/42, then stop.")).toEqual([
      { text: "See " },
      { link: "sona://meeting/42" },
      { text: ", then stop." },
    ]);
  });
});

describe("the two gestures", () => {
  const press = (key: string, shiftKey = false) => {
    let prevented = false;
    let fired = false;
    const event = {
      key,
      shiftKey,
      preventDefault: () => {
        prevented = true;
      },
    };
    return { event, prevented: () => prevented, fired: () => fired };
  };

  test("Enter sends, Shift+Enter opens a line", () => {
    let sends = 0;
    const send = composerKeys(() => {
      sends += 1;
    });

    const enter = press("Enter");
    send(enter.event);
    expect(sends).toBe(1);
    expect(enter.prevented()).toBe(true);

    const shifted = press("Enter", true);
    send(shifted.event);
    expect(sends).toBe(1);
    expect(shifted.prevented()).toBe(false);
  });

  test("Escape closes the sheet and nothing else does", () => {
    let closes = 0;
    const close = sheetKeys(() => {
      closes += 1;
    });

    close(press("Escape").event);
    expect(closes).toBe(1);

    for (const key of ["Enter", "Tab", "a"]) {
      close(press(key).event);
    }
    expect(closes).toBe(1);
  });
});

/* The rail's chat row and the column are one fold, so the shell is where that
 * is checkable: the row's press has to move the same boolean the column reads,
 * and the column's width has to come off the two columns already there. */
const shell = (chatOpen: boolean): string => {
  const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { __TAURI_OS_PLUGIN_INTERNALS__: { os_type: "macos" } },
  });
  try {
    return paint(
      <AppContent
        onboardingStep="done"
        onAccessibilityComplete={noop}
        onModelSelected={noop}
        direction="ltr"
        currentSection="overview"
        onSectionChange={noop}
        onOpenMeeting={noop}
        onOpenRecorder={noop}
        loadingLabel="Loading"
        meetingInvalidation={0}
        meetingNavigationRequest={null}
        meetingStartRequest={0}
        personRequest={null}
        organizationRequest={null}
        dictationRequest={null}
        commandOpen={false}
        commandActions={[]}
        commandSeed={null}
        agentPanel={{
          enabled: true,
          paired: true,
          remoteIntelligence: true,
        }}
        chatOpen={chatOpen}
        onChatOpenChange={noop}
        onCommandOpenChange={noop}
        onCommandOpen={noop}
      />,
    );
  } finally {
    if (restore) Object.defineProperty(globalThis, "window", restore);
    else Reflect.deleteProperty(globalThis, "window");
  }
};

describe("the shell's one fold", () => {
  test("the column is mounted beside the pane, once, whether open or not", () => {
    for (const open of [false, true]) {
      const markup = shell(open);
      const columnAt = markup.indexOf('data-slot="chat-sheet"');

      expect(occurrences(markup, 'data-slot="chat-sheet"')).toBe(1);
      /* After the pane closes, which is the one place it is a column of the
       * window rather than something laid over the page. */
      expect(columnAt).toBeGreaterThan(markup.indexOf("</main>"));
    }
  });

  test("opening it narrows the page instead of covering it", () => {
    const closed = shell(false);
    const open = shell(true);

    // The rail's words are what pay for the column: 220 becomes 48.
    expect(closed).toContain("w-[220px]");
    expect(open).toContain("w-[48px]");
    /* The pane is what is left, and it is left to flex into it: no width of
     * its own anywhere, so 900 - 48 - 340 is arithmetic rather than a number
     * somebody has to keep in step. */
    const pane = /<main class="([^"]*)"/.exec(open)?.[1] ?? "";
    expect(pane).toContain("flex-1");
    expect(pane).toContain("min-w-0");
    expect(pane).not.toMatch(/\bw-\[/);
  });

  test("the rail's row is the door and the column's X is the way back", () => {
    // Closed: one row, saying the region it opens is not showing.
    expect(shell(false)).toContain('aria-expanded="false"');
    expect(occurrences(shell(false), 'data-slot="chat-close"')).toBe(1);
    /* Open: the row stays where it is and says so, and the same single X owns
     * the way back out. A second closer beside the column is the duplication
     * this fold exists without. */
    expect(occurrences(shell(true), 'data-slot="chat-rail-row"')).toBe(1);
    expect(shell(true)).toContain('aria-expanded="true"');
    expect(occurrences(shell(true), 'data-slot="chat-close"')).toBe(1);
  });

  /* The old surface is gone from the shell entirely: no second webview to open
   * and no command left that would open one. */
  test("no window-opening command survives on the chat path", () => {
    expect("agentPanelOpen" in commands).toBe(false);
    expect("agentPanelClose" in commands).toBe(false);
  });
});

describe("the sheet's Ask turn", () => {
  test("packs only a paired Ask turn with remote intelligence consent", async () => {
    const original = {
      pack: commands.sonaQueryPack,
      send: commands.agentPanelSendTurn,
    };
    const packedQuestions: string[] = [];
    const sent: Array<{
      workspace: string;
      contextPack: string | null;
      toolsAllowed: boolean;
    }> = [];
    commands.sonaQueryPack = async (question) => {
      packedQuestions.push(question);
      return {
        status: "ok",
        data: {
          schema_version: 1,
          pack: "meeting quotes",
          sources: [
            {
              kind: "meeting",
              id: "m1",
              title: "Decisions",
              snippet: "We chose the launch date.",
              when_utc_ms: 1,
              link: "sona://meeting/m1",
            },
          ],
        },
      };
    };
    commands.agentPanelSendTurn = async (request) => {
      sent.push({
        workspace: request.workspace,
        contextPack: request.context_pack,
        toolsAllowed: request.tools_allowed,
      });
      return {
        status: "ok",
        data: {
          invalidation_id: 1,
          relay_status: "ready",
          conversation_id: "c1",
          conversation: [],
          turn: null,
          proposal: null,
        },
      };
    };
    const cases = [
      {
        workspace: "sona_chat",
        gate: { paired: true, remoteIntelligence: true },
      },
      {
        workspace: "sona_chat",
        gate: { paired: true, remoteIntelligence: false },
      },
      {
        workspace: "sona_chat",
        gate: { paired: false, remoteIntelligence: true },
      },
      {
        workspace: "sona_config",
        gate: { paired: true, remoteIntelligence: true },
      },
    ] as const;
    const searchedCorpus: boolean[] = [];
    try {
      for (const [index, turn] of cases.entries()) {
        const outcome = await sendSheetTurn({
          message: `question ${index}`,
          locale: "en",
          workspace: turn.workspace,
          gate: turn.gate,
        });
        searchedCorpus.push(outcome.searchedCorpus);
      }
    } finally {
      commands.sonaQueryPack = original.pack;
      commands.agentPanelSendTurn = original.send;
    }

    expect(packedQuestions).toEqual(["question 0"]);
    /* The grant is the gate: the model may ask this Mac to run Sona tools on
     * exactly the turns that carry a pack, and a settings turn never does. */
    expect(sent).toEqual([
      {
        workspace: "sona_chat",
        contextPack: "meeting quotes",
        toolsAllowed: true,
      },
      { workspace: "sona_chat", contextPack: null, toolsAllowed: false },
      { workspace: "sona_chat", contextPack: null, toolsAllowed: false },
      { workspace: "sona_config", contextPack: null, toolsAllowed: false },
    ]);
    expect(searchedCorpus).toEqual([true, false, false, false]);
  });

  test("retrying a failed question creates a fresh turn", async () => {
    const original = commands.agentPanelSendTurn;
    const turnIds: string[] = [];
    commands.agentPanelSendTurn = async (request) => {
      turnIds.push(request.turn_id);
      return {
        status: "ok",
        data: {
          invalidation_id: 1,
          relay_status: "ready",
          conversation_id: "c1",
          conversation: [],
          turn: null,
          proposal: null,
        },
      };
    };
    try {
      await sendSheetTurn({
        message: "What did we decide?",
        locale: "en",
        workspace: "sona_chat",
        gate: { paired: false, remoteIntelligence: false },
      });
      await sendSheetTurn({
        message: "What did we decide?",
        locale: "en",
        workspace: "sona_chat",
        gate: { paired: false, remoteIntelligence: false },
      });
    } finally {
      commands.agentPanelSendTurn = original;
    }

    expect(turnIds).toHaveLength(2);
    expect(turnIds[0]).not.toBe(turnIds[1]);
  });
});

describe("the palette's Ask row", () => {
  /* It builds the pack and sends the turn; the sheet is opened by the shell.
   * A second window is never asked for, which is the whole cutover. */
  test("asks the backend for a pack and a turn, and opens nothing", async () => {
    const original = {
      pack: commands.sonaQueryPack,
      send: commands.agentPanelSendTurn,
    };
    const calls: string[] = [];
    commands.sonaQueryPack = async () => {
      calls.push("pack");
      return {
        status: "ok",
        data: {
          schema_version: 1,
          question: "pricing",
          pack: "quotes",
          sources: [],
          truncated: false,
        },
      };
    };
    commands.agentPanelSendTurn = async (request) => {
      calls.push(`send:${request.workspace}:${request.context_pack ?? ""}`);
      return {
        status: "ok",
        data: {
          invalidation_id: 1,
          relay_status: "ready",
          conversation_id: "c1",
          conversation: [],
          turn: null,
          proposal: null,
        },
      };
    };
    try {
      expect(
        await askSona("pricing", "en", {
          enabled: true,
          paired: true,
          remoteIntelligence: true,
        }),
      ).toBe("sent");
    } finally {
      commands.sonaQueryPack = original.pack;
      commands.agentPanelSendTurn = original.send;
    }

    expect(calls).toEqual(["pack", "send:sona_chat:quotes"]);
  });
});

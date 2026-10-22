import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import { buildCommandActions } from "@/App";
import { usePromptShellStore } from "@/components/settings/meetings/promptTargets";
import {
  groupPaletteActions,
  isCommandPaletteChord,
  type CommandPaletteAction,
} from "./commandPaletteActions";
import { Sidebar } from "./Sidebar";
import {
  askSona,
  ASK_VALUE,
  canAsk,
  groupQueryRows,
  openRow,
  paletteFilter,
  rowValue,
  searchCorpus,
  SEARCH_LIMIT,
} from "./commandPaletteSearch";
import {
  commands,
  type AgentPanelSendTurnRequestV1,
  type AgentPanelStatusV1,
  type QueryRow,
} from "@/bindings";

/* The shell's standing proof.
 *
 * There is no DOM in this suite — every component test in this repo renders to
 * static markup — so the palette itself is not here: it is a Radix dialog and
 * portals to a document that does not exist. What is here is the pair of pure
 * rules the palette is built on, and the sidebar's rendered markup. */

const localeFile = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localeFile, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

/** One keydown, as the four fields the predicate reads. */
const chord = (
  overrides: Partial<Parameters<typeof isCommandPaletteChord>[0]> = {},
) => ({ key: "k", metaKey: true, ctrlKey: false, repeat: false, ...overrides });

describe("the palette chord", () => {
  test("Cmd+K and Ctrl+K both summon it", () => {
    expect(isCommandPaletteChord(chord())).toBe(true);
    expect(
      isCommandPaletteChord(chord({ metaKey: false, ctrlKey: true })),
    ).toBe(true);
  });

  /* The flicker, stated as a rule. The chord toggles, so every keydown the
   * listener accepts is one open-or-close. A held key repeats keydown at the
   * OS repeat rate, which is why holding it used to strobe the palette rather
   * than leave it open — and why an accepted repeat is a bug, not a nicety. */
  test("an auto-repeat is not a second press", () => {
    expect(isCommandPaletteChord(chord({ repeat: true }))).toBe(false);
    expect(
      isCommandPaletteChord(
        chord({ metaKey: false, ctrlKey: true, repeat: true }),
      ),
    ).toBe(false);
  });

  test("the modifier is required and the letter is not case-sensitive", () => {
    expect(isCommandPaletteChord(chord({ key: "K" }))).toBe(true);
    expect(isCommandPaletteChord(chord({ metaKey: false }))).toBe(false);
    expect(isCommandPaletteChord(chord({ key: "j" }))).toBe(false);
    expect(isCommandPaletteChord(chord({ key: "Escape" }))).toBe(false);
  });
});

const action = (
  id: string,
  group: CommandPaletteAction["group"],
): CommandPaletteAction => ({
  id,
  group,
  label: id,
  run: () => undefined,
});

describe("palette grouping", () => {
  test("every action survives grouping exactly once", () => {
    const actions = [
      action("a", "navigation"),
      action("b", "actions"),
      action("c", "navigation"),
    ];
    expect(
      groupPaletteActions(actions).flatMap((section) =>
        section.items.map((item) => item.id),
      ),
    ).toEqual(["a", "c", "b"]);
  });
});

describe("prompt palette action", () => {
  test("routes to the prompt library and requests its editor", () => {
    const destinations: unknown[] = [];
    usePromptShellStore.setState({
      newPromptRequest: 0,
      handledNewPromptRequest: 0,
    });
    const actions = buildCommandActions({
      t: (key) => key,
      agentEnabled: false,
      isMacos: false,
      onNavigate: (section, target) => destinations.push({ section, target }),
      onNewMeeting: () => {},
      onImportAudio: () => {},
      onImportMeeting: () => {},
      onOpenRecordings: () => {},
      onOpenAgent: () => {},
      onOpenRecorder: () => {},
    });

    actions.find((item) => item.id === "action-new-prompt")?.run();

    expect(destinations).toEqual([
      { section: "settings", target: { tab: "prompts" } },
    ]);
    expect(usePromptShellStore.getState().newPromptRequest).toBe(1);
  });
});

/* `useOsType` reads the Tauri OS plugin's global during render, and there is
 * no DOM here, so the global is planted for exactly the synchronous render
 * that needs it and then put back.
 *
 * Module scope would be the obvious place and is the wrong one: Motion decides
 * whether a render is a client render by whether `window` existed when it was
 * imported, so a global left standing here changes how lib/motion's own suite
 * renders in the same process. It did, measurably, before this was scoped. */
const renderSidebar = (
  activeSection: "overview" | "meetings" = "overview",
  /** Whether the chat column has the width, which is the rail's only fold. */
  collapsed = false,
) => {
  const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { __TAURI_OS_PLUGIN_INTERNALS__: { os_type: "macos" } },
  });
  try {
    return renderToStaticMarkup(
      /* Every window root mounts one `TooltipProvider`, and the collapsed rail
       * is the state that needs it: a glyph with no word beside it names itself
       * through Radix, which throws without a provider above it. */
      <I18nextProvider i18n={i18n}>
        <TooltipProvider>
          <Sidebar
            collapsed={collapsed}
            currentSection={activeSection}
            onSectionChange={() => undefined}
            onOpenCommand={() => undefined}
            agentPanel={{ enabled: true, paired: true }}
            chatOpen={false}
            onOpenChat={() => undefined}
          />
        </TooltipProvider>
      </I18nextProvider>,
    );
  } finally {
    if (restore) Object.defineProperty(globalThis, "window", restore);
    else Reflect.deleteProperty(globalThis, "window");
  }
};

describe("Sidebar", () => {
  /* Destinations are the nav landmark's rows and nothing else. The rail's two
   * doors — Search and Chat — sit above it: a button that opens a palette or a
   * column is not a place, and one parked between two destinations reads as a
   * third. Scoping the match to `<nav>` is what states that. */
  test("exposes every destination as a named navigation button", () => {
    const markup = renderSidebar();
    const nav = markup.slice(markup.indexOf("<nav"), markup.indexOf("</nav>"));
    const labels = [...nav.matchAll(/>([A-Za-z ]+)<\/button>/g)].map(
      (match) => match[1],
    );
    expect(new Set(labels)).toEqual(new Set([
      "Capture",
      "Library",
      "Meetings",
      "People",
      "Settings",
    ]));
    expect(nav).not.toContain('data-slot="chat-rail-row"');
  });

  /* aria-current is the selection; the styling reads the same current route. */
  test("aria-current names exactly one route", () => {
    const markup = renderSidebar("meetings");
    expect([...markup.matchAll(/aria-current="page"/g)]).toHaveLength(1);
    expect(markup).toMatch(/aria-current="page"[^>]*>.*?Meetings<\/button>/);
  });

  test("the search row is a button that names the palette and shows its chord", () => {
    const markup = renderSidebar();
    // The chord is spelled once, whole, on the row that opens the palette.
    expect(markup).toMatch(
      /<button[^>]*aria-label="Search"[^>]*>(?:(?!<\/button>).)*\u2318 K/,
    );
    expect([...markup.matchAll(/\u2318 K/g)]).toHaveLength(1);
  });

});

describe("the rail, collapsed for the chat column", () => {
  test("every destination survives as a glyph that still says its name", () => {
    const markup = renderSidebar("overview", true);

    for (const name of ["Capture", "Library", "Meetings", "People", "Settings"])
      expect(markup).toContain(`aria-label="${name}"`);
  });

  test("the selected route is still the selected route", () => {
    const markup = renderSidebar("meetings", true);

    expect([...markup.matchAll(/aria-current="page"/g)]).toHaveLength(1);
    expect(markup).toMatch(/aria-current="page"[^>]*Meetings/);
  });

});

/* ⌘K's second half: the corpus. The palette itself still cannot be rendered
 * here — Radix portals to a document this runner does not have — so what is
 * pinned is every rule the surface reads: how a page becomes sections, what the
 * list may never filter away, when the ask row exists, and what a chosen row
 * and a chosen ask actually do to the backend. */

const WHEN = 1_786_699_920_000;

const queryRow = (
  kind: QueryRow["kind"],
  id: string,
  when_utc_ms = WHEN,
): QueryRow => ({
  kind,
  id,
  title: `${id} title`,
  snippet: `${id} snippet`,
  when_utc_ms,
  link: `sona://${kind}/${id}`,
});

const PANEL_STATUS: AgentPanelStatusV1 = {
  invalidation_id: 1,
  relay_status: "ready",
  conversation_id: "conversation-1",
  conversation: [],
  turn: null,
  unsaved: false,
  proposal: null,
};

describe("what a page of the query plane looks like in the palette", () => {
  test("sections come in one order, and a kind nothing matched has none", () => {
    const sections = groupQueryRows([
      queryRow("loop", "l-1"),
      queryRow("meeting", "m-1"),
      queryRow("dictation", "7"),
    ]);

    expect(sections.map((section) => section.kind)).toEqual([
      "meeting",
      "dictation",
      "loop",
    ]);
  });

  /* Relevance decided membership; recency decided the page order
   * (`query/mod.rs`). Re-sorting inside a section would be this surface
   * inventing a second answer to a question the plane refuses to guess at. */
  test("the plane's page order survives inside a section", () => {
    const [meetings] = groupQueryRows([
      queryRow("meeting", "newest", WHEN),
      queryRow("meeting", "older", WHEN - 5_000),
      queryRow("meeting", "oldest", WHEN - 9_000),
    ]);

    expect(meetings.rows.map((row) => row.id)).toEqual([
      "newest",
      "older",
      "oldest",
    ]);
  });

  /* `QueryRowKind` declares six nouns; no scope produces these two, so a
   * section for them would be a heading that can never have a row under it. */
  test("the kinds no scope produces are not sections", () => {
    expect(
      groupQueryRows([queryRow("series", "s-1"), queryRow("receipt", "r-1")]),
    ).toEqual([]);
  });


  /* A meeting can come back from the semantic half of the plane without sharing
   * one letter with what was typed. cmdk would score that zero and hide it. */
  test("a plane row is never filtered out; an action keeps cmdk's own score", () => {
    const row = queryRow("meeting", "m-1");
    expect(paletteFilter(rowValue(row), "what did I promise Steven", [])).toBe(
      1,
    );
    expect(paletteFilter(ASK_VALUE, "what did I promise Steven", [])).toBe(1);
    expect(paletteFilter("Import audio", "import audio", [])).toBeGreaterThan(
      0,
    );
    expect(paletteFilter("Import audio", "zzzzzz", [])).toBe(0);
  });

  test("a row's value is its address, so no two rows collide", () => {
    expect(rowValue(queryRow("meeting", "m-1"))).not.toBe(
      rowValue(queryRow("dictation", "m-1")),
    );
  });
});

describe("the ask row", () => {
  const ready = { enabled: true, paired: true, remoteIntelligence: true };

  test("needs text as well as the three settings", () => {
    expect(canAsk("what did I promise Steven", ready)).toBe(true);
    expect(canAsk("   ", ready)).toBe(false);
    expect(canAsk("", ready)).toBe(false);
  });

  /* Every combination, because the row's whole job is to be absent unless all
   * three are yes: an unpaired panel has nowhere to send the question, and a
   * machine that has not consented to remote meeting intelligence may not send
   * transcript quotes anywhere at all — the pack is verbatim corpus text. */
  test("only all three yeses offer it", () => {
    const facts = [true, false];
    for (const enabled of facts) {
      for (const paired of facts) {
        for (const remoteIntelligence of facts) {
          const panel = { enabled, paired, remoteIntelligence };
          expect(canAsk("steven", panel)).toBe(
            enabled && paired && remoteIntelligence,
          );
        }
      }
    }
  });

});

describe("what the palette asks the backend", () => {
  test("one search of every scope, one page, no cursor", async () => {
    const original = commands.sonaQuerySearch;
    const calls: unknown[][] = [];
    commands.sonaQuerySearch = async (scope, query, limit, cursor) => {
      calls.push([scope, query, limit, cursor]);
      return {
        status: "ok",
        data: {
          schema_version: 1,
          entries: [queryRow("meeting", "m-1")],
          next_cursor: null,
          reason: null,
        },
      };
    };

    try {
      const outcome = await searchCorpus("steven");
      expect(calls).toEqual([["all", "steven", SEARCH_LIMIT, null]]);
      expect(outcome).toEqual({
        status: "rows",
        rows: [queryRow("meeting", "m-1")],
      });
    } finally {
      commands.sonaQuerySearch = original;
    }
  });

  /* Unavailable, invalid, unknown cursor: nothing a reader of a search box can
   * act on differently, so the surface reads one failure. */
  test("a refused search is one failure, not four", async () => {
    const original = commands.sonaQuerySearch;
    commands.sonaQuerySearch = async () => ({
      status: "error",
      error: "unavailable",
    });

    try {
      expect(await searchCorpus("steven")).toEqual({ status: "failed" });
    } finally {
      commands.sonaQuerySearch = original;
    }
  });

  test("choosing a row hands its address to the one dispatch", async () => {
    const original = commands.sonaOpenLink;
    const opened: string[] = [];
    commands.sonaOpenLink = async (link) => {
      opened.push(link);
      return true;
    };

    try {
      await openRow(queryRow("loop", "m-1:loop:abc"));
      // The loop's meeting is derived in `deeplink.rs`, never here.
      expect(opened).toEqual(["sona://loop/m-1:loop:abc"]);
    } finally {
      commands.sonaOpenLink = original;
    }
  });
});

describe("asking Sona", () => {
  const ready = { enabled: true, paired: true, remoteIntelligence: true };

  test("the turn carries the pack that was just built, and opens no window", async () => {
    const originalPack = commands.sonaQueryPack;
    const originalSend = commands.agentPanelSendTurn;
    const sent: AgentPanelSendTurnRequestV1[] = [];
    commands.sonaQueryPack = async (question) => ({
      status: "ok",
      data: {
        schema_version: 1,
        pack: `sona context pack 1\nquestion: ${question}\nquotes: 1 of 1`,
        sources: [queryRow("meeting", "m-1")],
      },
    });
    commands.agentPanelSendTurn = async (request) => {
      sent.push(request);
      return { status: "ok", data: PANEL_STATUS };
    };

    try {
      expect(await askSona("what did I promise Steven", "de", ready)).toBe(
        "sent",
      );
      expect(sent).toHaveLength(1);
      expect(sent[0].message).toBe("what did I promise Steven");
      expect(sent[0].workspace).toBe("sona_chat");
      expect(sent[0].locale).toBe("de");
      expect(sent[0].context_pack).toBe(
        "sona context pack 1\nquestion: what did I promise Steven\nquotes: 1 of 1",
      );
      /* The grant rides the same gate as the pack; a gate that is off sends
       * no turn at all (below), so neither entry point grants without it. */
      expect(sent[0].tools_allowed).toBe(true);
    } finally {
      commands.sonaQueryPack = originalPack;
      commands.agentPanelSendTurn = originalSend;
    }
  });

  /* Without evidence the model would answer from its own priors and cite
   * nothing, which is the failure "ask your history" exists to end. */
  test("no pack, no question", async () => {
    const originalPack = commands.sonaQueryPack;
    const originalSend = commands.agentPanelSendTurn;
    let sends = 0;
    commands.sonaQueryPack = async () => ({
      status: "error",
      error: "unavailable",
    });
    commands.agentPanelSendTurn = async () => {
      sends += 1;
      return { status: "ok", data: PANEL_STATUS };
    };

    try {
      expect(await askSona("what did I promise Steven", "en", ready)).toBe(
        "failed",
      );
      expect(sends).toBe(0);
    } finally {
      commands.sonaQueryPack = originalPack;
      commands.agentPanelSendTurn = originalSend;
    }
  });

  /* The gate is checked here and not only where the row is drawn: hiding the
   * row is what a reader sees, this is what makes the send impossible. Nothing
   * is built, so no corpus read happens either — a refused ask does not even
   * assemble the quotes it is not allowed to send. */
  test("without the meeting-intelligence consent nothing is built or sent", async () => {
    const original = {
      pack: commands.sonaQueryPack,
      send: commands.agentPanelSendTurn,
    };
    let packs = 0;
    let sends = 0;
    commands.sonaQueryPack = async () => {
      packs += 1;
      return { status: "error", error: "unavailable" };
    };
    commands.agentPanelSendTurn = async () => {
      sends += 1;
      return { status: "ok", data: PANEL_STATUS };
    };

    try {
      expect(
        await askSona("what did I promise Steven", "en", {
          ...ready,
          remoteIntelligence: false,
        }),
      ).toBe("refused");
      expect(await askSona("   ", "en", ready)).toBe("refused");
      expect(packs).toBe(0);
      expect(sends).toBe(0);
    } finally {
      commands.sonaQueryPack = original.pack;
      commands.agentPanelSendTurn = original.send;
    }
  });
});

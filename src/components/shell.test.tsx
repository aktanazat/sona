import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import { AppContent, type AppContentProps } from "@/App";
import { AgentRequestsSurface } from "./chat/agentRequestsPanel";

/* The shell: the rail's two doors, the three columns they move, and the drag
 * band the pane keeps.
 *
 * The chat's way in used to be a pill floating in the content pane's top-right
 * gutter, and that gutter is where every route draws its own primary action —
 * Library's "Import audio" sat under it. The door is a rail row now, in the one
 * surface of this window no page draws into, and the tests below pin the three
 * states it carries, where it sits, and that nothing floats in the pane at all
 * any more.
 *
 * The copy comes from the shipped en bundle rather than a fixture, so a missing
 * `chat.*` key fails here as a raw key in the markup instead of on a screen.
 * `renderToStaticMarkup` runs no effects and no events, which is why a press is
 * pinned against the handler the markup does or does not carry.
 *
 * The shell drags in the sidebar, which reads the OS off Tauri's window
 * globals, so a `window` has to exist for the length of this render — and only
 * for that length. Motion decides whether a render is a client render by
 * whether `window` existed when it was imported, so a global left standing at
 * module scope changes how other suites render in the same process. */

const localeFile = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

// SAFETY: the en bundle is repo-owned and check:translations pins these keys;
// the narrow states the shape this test reads, not a guess about foreign data.
const en = JSON.parse(fs.readFileSync(localeFile, "utf8")) as {
  chat: Record<"open" | "label" | "unpaired", string>;
  commandPalette: { open: string };
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

/** The opening tag of the one element carrying a slot, attributes included. */
const openTag = (markup: string, slot: string): string =>
  new RegExp(`<[a-z]+[^>]*data-slot="${slot}"[^>]*>`).exec(markup)?.[0] ?? "";

const shell = (
  section: AppContentProps["currentSection"],
  agentPanel: AppContentProps["agentPanel"] = {
    enabled: true,
    paired: true,
    remoteIntelligence: true,
  },
  /** The rail measures its traffic-light clearance from this; the pane's drag
   * band is not allowed to. */
  osType: "macos" | "windows" = "macos",
  chatOpen = false,
): string => {
  const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { __TAURI_OS_PLUGIN_INTERNALS__: { os_type: osType } },
  });
  try {
    return paint(
      <AppContent
        onboardingStep="done"
        onAccessibilityComplete={() => undefined}
        onModelSelected={() => undefined}
        direction="ltr"
        currentSection={section}
        onSectionChange={() => undefined}
        onOpenMeeting={() => undefined}
        onOpenRecorder={() => undefined}
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
        agentPanel={agentPanel}
        chatOpen={chatOpen}
        onChatOpenChange={() => undefined}
        onCommandOpenChange={() => undefined}
        onCommandOpen={() => undefined}
      />,
    );
  } finally {
    if (restore) Object.defineProperty(globalThis, "window", restore);
    else Reflect.deleteProperty(globalThis, "window");
  }
};

const OFF: AppContentProps["agentPanel"] = {
  enabled: false,
  paired: false,
  remoteIntelligence: false,
};

const UNPAIRED: AppContentProps["agentPanel"] = {
  enabled: true,
  paired: false,
  remoteIntelligence: false,
};

/** The rail's whole markup, which is where both of its doors live. */
const railOf = (markup: string): string =>
  markup.slice(
    markup.indexOf('data-slot="sidebar"'),
    markup.indexOf("</aside>"),
  );

/** The nav landmark, which is destinations and nothing else. */
const navOf = (markup: string): string => {
  const start = markup.indexOf("<nav");
  return markup.slice(start, markup.indexOf("</nav>", start));
};

describe("the rail's chat row, in its three states", () => {
  test("enabled and paired: one live row, named for the agent", () => {
    const markup = shell("overview");
    const row = openTag(markup, "chat-rail-row");

    expect(occurrences(markup, 'data-slot="chat-rail-row"')).toBe(1);
    expect(row).toContain("<button");
    expect(row).toContain(`aria-label="${en.chat.label}"`);
    /* The column is a region this button discloses, so the button says which
     * state it is in rather than leaving the reader to infer it. */
    expect(row).toContain('aria-expanded="false"');
    expect(row).not.toContain("aria-disabled");
    // The word beside the glyph, on the named rail.
    expect(markup).toContain(`>${en.chat.open}</button>`);
  });

  /* The shape the theme allows. `--radius-pill` is for status dots, the HUD
   * card and toggle tracks (styles/theme.css says so in as many words), and
   * the deleted pill was drawn at `rounded-full` in the pane's gutter. A rail
   * row takes the control radius every other rail row takes. */
  test("it is a rail row at the control radius, not a pill", () => {
    const row = openTag(shell("overview"), "chat-rail-row");

    expect(row).toContain("rounded-md");
    expect(row).not.toContain("rounded-full");
    // No plate of its own either: the rail's rows are flat until washed.
    expect(row).not.toContain("bg-raised");
    expect(row).not.toContain("border-gray-alpha-400");
  });

  /* Where it sits, and the whole reason it sits there: a door between two
   * destinations reads as a third destination. Search is the rail's other
   * door, so the two are grouped above the gap that separates them from the
   * pages, and neither is inside the nav landmark. */
  test("it is an action beside Search, never a row inside the nav", () => {
    const markup = shell("overview");
    const search = markup.indexOf(`aria-label="${en.commandPalette.open}"`);
    const row = markup.indexOf('data-slot="chat-rail-row"');

    expect(search).toBeGreaterThan(-1);
    expect(row).toBeGreaterThan(search);
    expect(row).toBeLessThan(markup.indexOf("<nav"));
    expect(navOf(markup)).not.toContain('data-slot="chat-rail-row"');
    // And it is in the rail rather than in the pane it used to float over.
    expect(railOf(markup)).toContain('data-slot="chat-rail-row"');
  });

  /* Open, the row is the pressed one, not the current one: `aria-current` on it
   * would announce the chat as a sixth destination and leave the actual route
   * unmarked. It also stays reachable — the collapsed rail keeps every one of
   * its rows — and the column's own X remains what closes the fold. */
  test("with the column showing: expanded and washed, never current", () => {
    const markup = shell("overview", undefined, "macos", true);
    const row = openTag(markup, "chat-rail-row");

    expect(row).toContain('aria-expanded="true"');
    expect(row).toContain("bg-gray-alpha-200");
    expect(row).not.toContain("aria-current");
    expect(row).not.toContain("aria-hidden");
    expect(row).not.toContain("inert");
    expect(row).not.toContain("pointer-events-none");
    expect(occurrences(markup, 'data-slot="chat-close"')).toBe(1);
  });

  /* Unpaired is the state a first run is in: the agent is on, nothing would
   * answer a turn yet. The row stays visible and inert — hiding it here would
   * make the fix undiscoverable — and the reason is the tooltip's, which is why
   * the sentence itself must not be printed into the row. */
  test("unpaired: inert, still focusable, and the reason is a tooltip", () => {
    const markup = shell("overview", UNPAIRED);
    const row = openTag(markup, "chat-rail-row");

    expect(row).toContain('aria-disabled="true"');
    /* `asChild` puts the trigger's behaviour on the row itself, so the row
     * carries the tooltip's own state attribute rather than a wrapper — and it
     * is inert without ever taking `disabled`, which is what keeps it
     * focusable and so keeps the reason reachable by keyboard. */
    expect(row).toContain('data-state="closed"');
    expect(row).not.toMatch(/\sdisabled(=|\s|>)/);
    /* An inert row claims no expanded state: it controls nothing. */
    expect(row).not.toContain("aria-expanded");
    // Said once, in the tooltip, which is portalled and so not in this markup.
    expect(markup).not.toContain(en.chat.unpaired);
    // Dimmed type rather than opacity, like every disabled row in the app,
    // and no hover wash on something that does nothing.
    expect(row).toContain("text-gray-800");
    expect(row).not.toContain("opacity-");
    expect(row).not.toContain("hover:bg-gray-alpha-100");
  });

  test("switched off by setting: no row at all, not a dimmed one", () => {
    const markup = shell("overview", OFF);

    expect(markup).not.toContain('data-slot="chat-rail-row"');
    expect(markup).not.toContain(en.chat.open);
    // The rail's other door is not the agent's, and it survives.
    expect(markup).toContain(`aria-label="${en.commandPalette.open}"`);
  });

  test("the row is a button in the rail, not a drag handle", () => {
    const row = openTag(shell("overview"), "chat-rail-row");

    expect(row).toContain("<button");
    expect(row).not.toContain("data-tauri-drag-region");
  });
});

/* The pane, now that nothing of the shell's floats in it.
 *
 * A control the shell parked in the pane's top-right gutter covered whichever
 * page action the route drew there, on every route at once. The rule the pill
 * broke is stated here as structure: between `<main>` and its scroll owner
 * there is the drag band and nothing else. */
describe("the pane belongs to the page", () => {
  test("nothing interactive sits between the pane and its scroll owner", () => {
    for (const section of ["overview", "settings", "history"] as const) {
      const markup = shell(section);
      const chrome = markup.slice(
        markup.indexOf("<main"),
        markup.indexOf('data-slot="page-scroll"'),
      );

      expect(chrome).toContain('data-slot="drag-band"');
      expect(chrome).not.toContain("<button");
      expect(chrome).not.toContain("<a ");
    }
  });

  /* The gutter the pill used to claim, named by its own numbers so a later
   * floating control cannot quietly reoccupy it. */
  test("no shell control claims the pane's top-right gutter", () => {
    const markup = shell("overview");

    expect(markup).not.toContain("end-[28px]");
    expect(markup).not.toContain("top-[7px]");
  });

  /* `relative` is still load-bearing: the drag band is absolutely positioned
   * against the pane. */
  test("the pane is the positioned box the drag band measures from", () => {
    const markup = shell("overview");
    const main = /<main class="([^"]*)"/.exec(markup)?.[1] ?? "";

    expect(main).toContain("relative");
    expect(openTag(markup, "drag-band")).toContain("absolute inset-x-0 top-0");
  });

  /* The banner strip may not grow into the drag band: a banner over it is a
   * window nobody can move. */
  test("the banner strip starts below the drag band", () => {
    expect(shell("overview")).toContain("pt-12");
  });
});

/* The shell's three columns, which is what "the chat is part of the window"
 * comes down to.
 *
 * The window is a fixed 900x800 (src-tauri/src/lib.rs) and nothing in the app
 * resizes it, so the chat's 340 has to come out of the two columns already
 * there: the rail gives up its words for 48 and the page keeps 512. The
 * arithmetic belongs to the shell rather than to any one of the three, which
 * is why all three are read here. */
describe("the shell's three columns", () => {
  test("closed: the named rail, the full page, and a column 0 wide", () => {
    const markup = shell("overview");

    expect(markup).toContain("w-[220px]");
    expect(markup).not.toContain("w-[48px]");
    /* Mounted so it has somewhere to open from, and taking no width until it
     * does — the page is the full pane, exactly as it was before any of this. */
    expect(occurrences(markup, 'data-slot="chat-sheet"')).toBe(1);
    expect(markup).toContain("pointer-events-none w-0");
  });

  test("open: rail 48, page between, chat 340 — in that order", () => {
    const markup = shell("overview", undefined, "macos", true);
    const rail = markup.indexOf('data-slot="sidebar"');
    const pane = markup.indexOf("<main");
    const column = markup.indexOf('data-slot="chat-sheet"');

    expect(markup).toContain("w-[48px]");
    expect(markup).not.toContain("w-[220px]");
    expect(markup).toContain("w-[340px]");
    // Source order is column order: rail, then pane, then chat.
    expect(rail).toBeLessThan(pane);
    expect(pane).toBeLessThan(column);
    /* Beside the pane and not inside it. A column that opens before `</main>`
     * is a column whose width was never taken off the page, which is a page
     * underneath the chat rather than next to it — the whole regression this
     * cutover is about. */
    expect(column).toBeGreaterThan(markup.indexOf("</main>"));
  });

  /* An agent switched off has no column, so there is nothing for the rail to
   * make room for and nothing to narrow the page for. */
  test("switched off, an open fold narrows nothing", () => {
    const markup = shell("overview", OFF, "macos", true);

    expect(markup).toContain("w-[220px]");
    expect(markup).not.toContain("w-[48px]");
    expect(markup).toContain("pointer-events-none w-0");
  });

  /* The pane's own handle is not the chat's: the drag band is 512 wide with the
   * column open, and it is still the pane's full width, because the column is
   * beside the pane rather than over part of it. */
  test("open: the pane keeps one full-width drag band", () => {
    const markup = shell("overview", undefined, "macos", true);

    expect(occurrences(markup, 'data-slot="drag-band"')).toBe(1);
    expect(markup).toContain("absolute inset-x-0 top-0 z-0 h-12");
  });
});

/* The band above every page's first heading is also the window's handle, which
 * is why it is pinned here: 42px of chrome and one hit-test order.
 *
 * The mechanism itself is Tauri's: a mousedown whose target carries
 * `data-tauri-drag-region` invokes `plugin:window|start_dragging`, which
 * `core:window:allow-start-dragging` in capabilities/default.json permits.
 * Nothing else in the pane claims a pixel of the band, so if the attribute
 * leaves this strip the window stops moving — the regression these tests
 * exist for. */
describe("the pane's drag band", () => {
  test("the strip carries the drag region on every route", () => {
    for (const section of ["overview", "settings"] as const) {
      const markup = shell(section);
      const band = openTag(markup, "drag-band");

      expect(occurrences(markup, 'data-slot="drag-band"')).toBe(1);
      expect(band).toContain("data-tauri-drag-region");
      // The pane's own top band: full width, page-content height, behind
      // anything a page positions over it. A strip that stops being those is a
      // strip nobody can grab.
      expect(band).toContain("absolute inset-x-0 top-0 z-0 h-12");
      // Inside the pane and above its scroll owner, so it neither scrolls
      // away nor lands twelve times.
      const index = markup.indexOf('data-slot="drag-band"');
      expect(index).toBeGreaterThan(markup.indexOf("<main"));
      expect(index).toBeLessThan(markup.indexOf('data-slot="page-scroll"'));
    }
  });

  /* The rail's clearance is platform-shaped; the pane's handle is not. A drag
   * region is inert where the native title bar survives, so gating it would
   * buy a branch and lose nothing. */
  test("it is unconditional, not macOS-only", () => {
    const band = openTag(shell("overview", undefined, "windows"), "drag-band");

    expect(band).toContain("data-tauri-drag-region");
  });

  /* Turning the agent off removes the rail's chat row. It must not remove the
   * handle. */
  test("it survives the agent being switched off", () => {
    expect(openTag(shell("overview", OFF), "drag-band")).toContain(
      "data-tauri-drag-region",
    );
  });
});

/* The native main window is fixed at 900 × 800. The page scroll owner must be
 * allowed to shrink inside that fixed flex column; otherwise Settings grows
 * past the pane and its lower controls are clipped instead of scrollable. */
describe("the fixed-window page scroll owner", () => {
  test("Settings has a shrinkable vertical scroll owner", () => {
    const scrollOwner = openTag(shell("settings"), "page-scroll");

    expect(scrollOwner).toContain("min-h-0");
    expect(scrollOwner).toContain("overflow-y-auto");
  });
});
describe("chat agent requests", () => {
  const surface = (
    bridgeEnabled: boolean,
    pendingCount: number,
    open: boolean,
  ) =>
    paint(
      <AgentRequestsSurface
        bridgeEnabled={bridgeEnabled}
        pendingCount={pendingCount}
        open={open}
        onOpen={() => {}}
        onClose={() => {}}
      >
        <span data-testid="agent-request-queue">Queue</span>
      </AgentRequestsSurface>,
    );

  test("stays absent when the bridge is off or the queue is empty", () => {
    expect(surface(false, 2, false)).toBe("");
    expect(surface(true, 0, false)).toBe("");
  });

  test("shows the pending count before opening the queue", () => {
    const found = surface(true, 2, false);

    expect(found).toContain('data-slot="agent-requests-entry"');
    expect(found).toContain("Pending replies");
    expect(found).toContain(">2<");
    expect(found).not.toContain('data-testid="agent-request-queue"');
  });

  test("replaces the entry with the reply queue after opening", () => {
    const found = surface(true, 2, true);

    expect(found).toContain('data-slot="agent-requests-panel"');
    expect(found).toContain('data-testid="agent-request-queue"');
    expect(found).not.toContain('data-slot="agent-requests-entry"');
  });
});

import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { installTauriMock } from "./support/tauri-mock";
import {
  APP_SETTINGS,
  CAPTURE_AT_FULL_HEIGHT,
  HISTORY_ENTRIES,
  HISTORY_RECEIPTS,
  HISTORY_STATS,
} from "./support/tauri-fixtures";

/* The shell is Tailwind utilities on its components now, so nothing here
 * selects by class: every locator is a role or an accessible name, which is
 * also the part of the surface that is not allowed to change silently. The one
 * exception is the settings hub's own test id, which MeetingsSettings owns. */
const sidebarNav = (page: Page) =>
  page.getByRole("navigation", { name: "Main navigation" });
const palette = (page: Page) => page.getByRole("dialog");

const openApp = async (page: Page) => {
  await installTauriMock(page);
  await page.goto("/");
  // The rail is the app's first paint; waiting on it replaces every sleep.
  await expect(
    sidebarNav(page).getByRole("button", { name: "Capture", exact: true }),
  ).toBeVisible();
};

/* One page of the query plane, one row per kind it produces. The shape is
 * `QuerySearchPage` in src/bindings.ts. */
const planeRow = (kind: string, id: string, title: string) => ({
  kind,
  id,
  title,
  snippet: `why ${title} is in front of you`,
  when_utc_ms: 1_786_699_920_000,
  link: `sona://${kind}/${id}`,
});

const SEARCH_PAGE = {
  schema_version: 1,
  entries: [
    planeRow("meeting", "meeting-1", "Weekly planning"),
    planeRow("person", "person-1", "Stephen Kowalski"),
    planeRow("dictation", "7", "A dictated note"),
    planeRow("loop", "meeting-1:loop:a", "Send the tier comparison"),
  ],
  next_cursor: null,
};

/* The ask row is gated on the panel toggle, a pairing and D14's consent, and
 * the install that reported this had all three. */
const PAIRED_SETTINGS = {
  ...APP_SETTINGS,
  agent_panel_paired: true,
  meeting_remote_intelligence_enabled: true,
};

test.describe("App shell", () => {
  test("the rail is five destinations with Settings on the bottom edge", async ({
    page,
  }) => {
    await openApp(page);

    const nav = sidebarNav(page);
    const boxes: Array<{ x: number; y: number; height: number }> = [];
    for (const name of [
      "Capture",
      "Library",
      "Meetings",
      "People",
      "Settings",
    ]) {
      const row = nav.getByRole("button", { name, exact: true });
      await expect(row).toBeVisible();
      const box = await row.boundingBox();
      if (!box) throw new Error(`${name} has no box`);
      boxes.push(box);
    }

    // Declaration order in the registry is reading order down the rail.
    for (let i = 1; i < boxes.length; i += 1) {
      expect(boxes[i].y).toBeGreaterThan(boxes[i - 1].y);
    }

    /* Settings is the one row nobody navigates to while working, so it sits
     * against the bottom edge rather than under People: the gap above it is
     * the rail's leftover height, not the row rhythm the other four keep. A
     * rail that lost its `mt-auto` fails here with five even gaps. */
    const gapAbove = (index: number) =>
      boxes[index].y - (boxes[index - 1].y + boxes[index - 1].height);
    expect(gapAbove(4)).toBeGreaterThan(gapAbove(3) + 40);
  });

  test("aria-current names the active route and follows it", async ({
    page,
  }) => {
    await openApp(page);

    const nav = sidebarNav(page);
    const capture = nav.getByRole("button", { name: "Capture", exact: true });
    const meetings = nav.getByRole("button", { name: "Meetings", exact: true });

    await expect(capture).toHaveAttribute("aria-current", "page");

    // Meetings is a first-class destination, not a segment inside Library.
    await meetings.click();
    await expect(meetings).toHaveAttribute("aria-current", "page");
    await expect(capture).not.toHaveAttribute("aria-current", "page");
  });

  test("the Settings row opens the hub on Essentials", async ({ page }) => {
    await openApp(page);

    await sidebarNav(page)
      .getByRole("button", { name: "Settings", exact: true })
      .click();
    await expect(page.getByTestId("settings-hub")).toBeVisible();

    /* Two tabs, and Debug is absent until the chord unlocks it. Five of the
     * seven tabs this hub used to carry are gone: General, Privacy, Agents,
     * Workflows and About are all Advanced now. */
    const tabs = page.getByRole("tablist", { name: "Settings" });
    await expect(tabs.getByRole("tab")).toHaveText(["Essentials", "Advanced"]);
    await expect(
      tabs.getByRole("tab", { name: "Essentials", exact: true }),
    ).toHaveAttribute("aria-selected", "true");
  });

  /* Meeting apps is the one row on this page that opens instead of toggling,
   * and the reason it earns a place among the essentials is that it stays
   * shut: six checkboxes, two switches and an Add button were a third of
   * Essentials before they went behind a summary. A static render can see the
   * markup; only a browser can press the thing and watch the checklist
   * arrive, so both halves are read here in order — closed on arrival, open
   * and populated on press. */
  test("the Meeting apps checklist arrives only when its summary is pressed", async ({
    page,
  }) => {
    await openApp(page);

    await sidebarNav(page)
      .getByRole("button", { name: "Settings", exact: true })
      .click();

    const disclosure = page
      .getByTestId("settings-essentials")
      .locator("details")
      .filter({ hasText: "Meeting apps" });
    const zoom = disclosure.getByRole("checkbox", {
      name: "Zoom",
      exact: true,
    });

    // Closed on arrival, so the list costs a reader one line and no scroll.
    await expect(disclosure).not.toHaveAttribute("open");

    await disclosure.locator("summary").click();

    await expect(disclosure).toHaveAttribute("open");
    await expect(zoom).toBeVisible();
  });

  /* With detection off, every control inside the checklist refuses, and the
   * closed row is the only place a reader can learn that without opening it.
   * The status shape is `DetectionStatus` in src/bindings.ts. */
  test("the Meeting apps row reads as inert while detection is off", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        detection_status_get: {
          eventSchemaVersion: 2,
          settings: {
            enabled: false,
            calendarEnabled: false,
            anyMicActivity: false,
            autoStartOnOpenPane: false,
            meetingApps: ["us.zoom.xos"],
          },
          calendarAccess: "not_determined",
          notificationAccess: "not_determined",
          inputDeviceActive: false,
          sonaHoldsInputDevice: false,
          suppressReason: "detection_disabled",
          countdown: null,
          runningMeetingApps: [],
          availableStopTriggers: [],
          inputDeviceReportingSuspect: false,
        },
      },
    });
    await page.goto("/");

    await sidebarNav(page)
      .getByRole("button", { name: "Settings", exact: true })
      .click();

    const disclosure = page
      .getByTestId("settings-essentials")
      .locator("details")
      .filter({ hasText: "Meeting apps" });
    await expect(disclosure).toHaveAttribute("data-disabled", "true");

    await disclosure.locator("summary").click();
    await expect(
      disclosure.getByRole("checkbox", { name: "Zoom", exact: true }),
    ).toBeDisabled();
  });

  test("shows the Debug shortcut in Advanced", async ({ page }) => {
    await openApp(page);

    await sidebarNav(page)
      .getByRole("button", { name: "Settings", exact: true })
      .click();
    await page.getByRole("tab", { name: "Advanced", exact: true }).click();

    /* Debug has no row and no link anywhere, so the one line that says how to
     * reach it is load-bearing. */
    await expect(
      page.getByText("Press \u2318\u21e7D to open the debug page."),
    ).toBeVisible();
  });

  /* A closed row's summary is the only place the watch list says what is in
   * it, so it has to account for every row. A tracker being typed has no name
   * yet, and a summary built from the named ones alone told a reader the
   * roster was smaller than it is. */
  test("the watch list summary counts the trackers that have no name yet", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        list_keyword_trackers: [
          { name: "Pricing", patterns: ["what does it cost"] },
          { name: "", patterns: [] },
          { name: "", patterns: [] },
        ],
      },
    });
    await page.goto("/");

    await sidebarNav(page)
      .getByRole("button", { name: "Settings", exact: true })
      .click();
    await page.getByRole("tab", { name: "Advanced", exact: true }).click();

    const watchList = page
      .locator("details")
      .filter({ hasText: "Keyword trackers" });
    await expect(watchList.locator("summary")).toContainText("Pricing +2");
  });

  test("the search row opens the command palette", async ({ page }) => {
    await openApp(page);

    await page.getByRole("button", { name: "Search", exact: true }).click();
    await expect(palette(page)).toBeVisible();
  });

  /* The regression that moved the chat's door into the rail.
   *
   * The door used to be a pill the shell mounted at `top-[7px] end-[28px]`
   * inside the content pane. That corner is not the shell's: every page puts
   * its own primary action at the top right of its title row, so the pill lay
   * across whichever control the route drew there. On Library the two boxes
   * measured 803–872 x 7–35 and 761–872 x 28–56 at the shipped window size —
   * a 69 x 7px overlap directly on "Import audio".
   *
   * So this measures the rule rather than the pill: no control the shell draws
   * may share a pixel with a control the page draws. The shell's controls are
   * the rail's rows plus anything in `main` outside the scroll owner, which is
   * exactly where a floating affordance would have to live to reoccupy that
   * gutter. Read at rest and again with the page scrolled to its bottom,
   * because a pane-level overlay collides with whatever scrolls under it. */
  test("no shell control overlaps a page control on Library", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 900, height: 800 });
    await installTauriMock(page, {
      responses: {
        get_settings: PAIRED_SETTINGS,
        get_app_settings: PAIRED_SETTINGS,
        history_entries: HISTORY_ENTRIES,
        history_stats: HISTORY_STATS,
        history_receipts: HISTORY_RECEIPTS,
      },
    });
    await page.goto("/");
    await sidebarNav(page)
      .getByRole("button", { name: "Library", exact: true })
      .click();
    await expect(page.getByTestId("history-import")).toBeVisible();

    const overlaps = (scrolled: boolean) =>
      page.evaluate((toBottom: boolean) => {
        // SAFETY: both slots are App.tsx's own rendered elements, and the
        // caller has already waited for the route's own button inside the
        // scroll owner.
        const pane = document.querySelector(
          '[data-slot="page-scroll"]',
        ) as HTMLElement;
        if (toBottom) pane.scrollTop = pane.scrollHeight;

        /* `:is()` because a descendant prefix in front of a bare comma list
         * only binds its first selector: `main button, input` is every input
         * in the document. */
        const CONTROLS =
          ':is(button, a[href], input, select, textarea, [role="button"])';
        const drawn = (node: Element) =>
          node.getClientRects().length > 0 &&
          node.getBoundingClientRect().width > 0;
        const named = (node: Element) =>
          node.getAttribute("data-testid") ??
          node.getAttribute("aria-label") ??
          ((node.textContent ?? "").trim().slice(0, 32) ||
            node.tagName.toLowerCase());
        const boxOf = (node: Element) => {
          const box = node.getBoundingClientRect();
          return {
            what: named(node),
            left: Math.round(box.left),
            top: Math.round(box.top),
            right: Math.round(box.right),
            bottom: Math.round(box.bottom),
          };
        };

        const railRows = Array.from(
          document.querySelectorAll(`[data-slot="sidebar"] ${CONTROLS}`),
        );
        /* Anything the shell floats over the page: `main`'s own controls that
         * are not inside the region the page scrolls in. The deleted pill was
         * the only member this set ever had. */
        const floating = Array.from(
          document.querySelectorAll(`main ${CONTROLS}`),
        ).filter((node) => node.closest('[data-slot="page-scroll"]') === null);
        const chrome = [...railRows, ...floating].filter(drawn).map(boxOf);
        const pageControls = Array.from(pane.querySelectorAll(CONTROLS))
          .filter(drawn)
          .map(boxOf);

        const collisions: string[] = [];
        for (const shellBox of chrome)
          for (const pageBox of pageControls)
            if (
              shellBox.left < pageBox.right &&
              pageBox.left < shellBox.right &&
              shellBox.top < pageBox.bottom &&
              pageBox.top < shellBox.bottom
            )
              collisions.push(
                `shell "${shellBox.what}" (${shellBox.left}–${shellBox.right} x ${shellBox.top}–${shellBox.bottom}) covers page "${pageBox.what}" (${pageBox.left}–${pageBox.right} x ${pageBox.top}–${pageBox.bottom})`,
              );

        return {
          chrome: chrome.length,
          pageControls: pageControls.length,
          scrolledTo: pane.scrollTop,
          scrolls: pane.scrollHeight - pane.clientHeight,
          collisions,
        };
      }, scrolled);

    for (const scrolled of [false, true]) {
      const report = await overlaps(scrolled);
      test.info().annotations.push({
        type: "boxes",
        description: `${scrolled ? "scrolled to bottom" : "at rest"}: ${report.chrome} shell controls against ${report.pageControls} page controls, pane at ${report.scrolledTo}/${report.scrolls}px`,
      });

      // The measurement has to have measured something: the page's own primary
      // action is in the set, and so are the rail's rows.
      expect(report.pageControls).toBeGreaterThan(0);
      expect(report.chrome).toBeGreaterThanOrEqual(7);
      expect(report.collisions, report.collisions.join("\n")).toEqual([]);
    }
  });
});

/* First run, which is the one screen every install sees and the one no unit
 * test can reach: the permission probe lives in a mount effect that calls into
 * the OS plugin, so the branch a reader lands on only exists in a browser.
 *
 * These replace a suite that read AccessibilityOnboarding.tsx as a string and
 * asserted substrings of it - green through any implementation that kept the
 * words. The mock counts every `invoke` by command in localStorage, so the
 * cost of the probe is observable here, at the boundary the storm crossed. */
test.describe("first run", () => {
  const FIRST_RUN = { ...APP_SETTINGS, onboarding_completed: false };
  const PERMISSION = "plugin:macos-permissions|check_accessibility_permission";
  const MICROPHONE = "plugin:macos-permissions|check_microphone_permission";

  const openFirstRun = async (page: Page, granted: boolean) => {
    await installTauriMock(page, {
      responses: {
        get_app_settings: FIRST_RUN,
        get_settings: FIRST_RUN,
        [PERMISSION]: granted,
        [MICROPHONE]: granted,
      },
    });
    await page.goto("/");
  };

  const probeCount = (page: Page, command: string) =>
    page.evaluate(
      (key: string) => Number(localStorage.getItem(`tauri-invoke:${key}`) ?? 0),
      command,
    );

  test("a permission asks for itself once, and only what is missing", async ({
    page,
  }) => {
    await openFirstRun(page, false);

    // One sentence under the title, then one row per permission still missing:
    // what it is, why it is needed, and the button that grants it.
    await expect(
      page.getByRole("heading", { name: "One-time setup", exact: true }),
    ).toBeVisible();
    const grants = page.getByRole("button", { name: "Grant permission" });
    await expect(grants).toHaveCount(2);
    await expect(
      page.getByRole("heading", { name: "Microphone access", exact: true }),
    ).toBeVisible();

    /* Waiting is the state this screen used to get stuck in. macOS shows the
     * consent dialog once ever, so after a denial there is nothing left to
     * click unless the row that is waiting carries both ways out itself: the
     * exact Settings pane, and a re-check that restarts a poll three failures
     * can stop for good. The microphone row is the first of the two. */
    await grants.first().click();
    await expect(page.getByText("Waiting…", { exact: true })).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Open System Settings", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Re-check", exact: true }),
    ).toBeVisible();
    // The row that is not waiting still asks the way it did.
    await expect(grants).toHaveCount(1);
  });

  test("the probe runs per mount, not per render of the shell", async ({
    page,
  }) => {
    await openFirstRun(page, true);

    // Granted, so this screen has nothing to ask and hands over to the model.
    await expect(
      page.getByRole("heading", {
        name: "Pick a transcription model",
        exact: true,
      }),
    ).toBeVisible();

    /* The number. Completing this step writes the audio device lists into the
     * settings store, which re-renders the shell above it; while the mount
     * effect depended on the callback that shell rebuilds every render, that
     * write re-ran the probe, which wrote again - fifty-three OS permission
     * calls in the second before React gave up with "Maximum update depth
     * exceeded", and a catch that blamed the permission check for it. Two is
     * the mount count in development, where StrictMode mounts twice. */
    expect(await probeCount(page, PERMISSION)).toBeLessThanOrEqual(4);
    expect(await probeCount(page, MICROPHONE)).toBeLessThanOrEqual(4);

    // The wrong sentence that loop raised, on the screen after it.
    await expect(page.getByText("Couldn't check permissions")).toHaveCount(0);
  });
});

/* The regression this slice exists for.
 *
 * ⌘K toggles, so every keydown the shell accepts is one open-or-close. Holding
 * the chord makes the OS repeat keydown at its repeat rate, and the listener
 * used to accept all of them: the palette strobed for as long as the chord was
 * held. Two more mechanisms piled onto the same press — the surface was behind
 * a lazy chunk with a null Suspense fallback, so the first chord painted
 * nothing at all until the chunk landed, and it then entered on a spring from
 * opacity 0. Anyone who pressed again during that gap toggled it shut.
 *
 * Each test below pins one of those: exactly one dialog per press, a held
 * chord that stays open on the same element, and a press that resolves on the
 * first frame with no second surface behind it. */
test.describe("the ⌘K palette does not flicker", () => {
  /** Marks the live dialog node, so a later assertion can tell a surviving
   * element from a replacement. React drops the property with the node. */
  const tagDialog = (page: Page) =>
    palette(page).evaluate((element) => {
      // SAFETY: the palette locator resolves the dialog element, which is an
      // HTMLElement in the page; evaluate types its argument as bare Element.
      (element as HTMLElement).dataset.flickerProbe = "same-node";
    });

  test("one press opens exactly one palette", async ({ page }) => {
    await openApp(page);

    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toBeVisible();
    await expect(palette(page)).toHaveCount(1);
    /* Nothing waits for a chunk any more, so the field owns the keyboard on
       the same frame the dialog appears. Radix marks the rest of the app
       aria-hidden while the modal is up, so this is the only combobox. */
    await expect(page.getByRole("combobox")).toBeFocused();
  });

  test("holding the chord keeps one palette open on the same element", async ({
    page,
  }) => {
    await openApp(page);

    /* Playwright sets `repeat` on every `down()` after the first for a key
       that is already held — the same flag the OS sets, which is the one the
       shell now drops. */
    await page.keyboard.down("Meta");
    await page.keyboard.down("k");
    await expect(palette(page)).toBeVisible();
    await tagDialog(page);

    for (let press = 0; press < 12; press += 1) {
      await page.keyboard.down("k");
    }
    await page.keyboard.up("k");
    await page.keyboard.up("Meta");

    await expect(palette(page)).toHaveCount(1);
    await expect(palette(page)).toHaveAttribute(
      "data-flicker-probe",
      "same-node",
    );
  });

  /* The rule itself, driven directly: a burst of synthesised repeats. This
     does not depend on how the harness models a held key, so it fails if the
     guard is removed even where `keyboard.down` stops setting `repeat`. */
  test("synthesised auto-repeats are ignored outright", async ({ page }) => {
    await openApp(page);

    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toBeVisible();
    await tagDialog(page);

    await page.evaluate(() => {
      for (let repeat = 0; repeat < 25; repeat += 1) {
        document.dispatchEvent(
          new KeyboardEvent("keydown", {
            key: "k",
            metaKey: true,
            repeat: true,
            bubbles: true,
            cancelable: true,
          }),
        );
      }
    });

    await expect(palette(page)).toHaveCount(1);
    await expect(palette(page)).toHaveAttribute(
      "data-flicker-probe",
      "same-node",
    );
  });

  test("a second real press closes it, and Escape does too", async ({
    page,
  }) => {
    await openApp(page);

    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toBeVisible();
    // The chord is still a toggle; only repeats stopped counting.
    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toHaveCount(0);

    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(palette(page)).toHaveCount(0);

    /* And the page underneath is reachable again: a modal dialog that failed
       to release focus would swallow this click. */
    const meetings = sidebarNav(page).getByRole("button", {
      name: "Meetings",
      exact: true,
    });
    await meetings.click();
    await expect(meetings).toHaveAttribute("aria-current", "page");
  });
});

test.describe("the palette's content", () => {
  test("groups destinations and actions, and filtering narrows to one", async ({
    page,
  }) => {
    await openApp(page);
    await page.keyboard.press("Meta+k");

    await expect(page.getByRole("group", { name: "Navigation" })).toBeVisible();
    await expect(page.getByRole("group", { name: "Actions" })).toBeVisible();

    const options = page.getByRole("option");
    const before = await options.count();
    expect(before).toBeGreaterThan(2);

    await page.getByRole("combobox").fill("Import audio");
    await expect(options).toHaveCount(1);
    await expect(options.first()).toHaveText(/Import audio/);

    /* Two characters in, the field is a corpus search as well as a filter, so
       the one sentence covers both halves: no command matched and the plane
       came back with nothing. "No commands found" would answer half the
       question the reader just asked. */
    await page.getByRole("combobox").fill("zzzzzz");
    await expect(options).toHaveCount(0);
    await expect(page.getByText("Nothing matched “zzzzzz”.")).toBeVisible();
  });

  /* One destination, one name. The palette used to label these two rows from
   * the section registry — "Overview" and "History" — for the destinations the
   * rail spells "Capture" and "Library", and the palette is the surface where
   * both spellings would have been readable at once. */
  test("destinations are named exactly as the rail names them", async ({
    page,
  }) => {
    await openApp(page);

    const railLabels = await sidebarNav(page)
      .getByRole("button")
      .allInnerTexts();
    expect(railLabels).toEqual([
      "Capture",
      "Library",
      "Meetings",
      "People",
      "Settings",
    ]);

    await page.keyboard.press("Meta+k");
    const destinations = page
      .getByRole("group", { name: "Navigation" })
      .getByRole("option");
    const paletteLabels = await destinations.allInnerTexts();

    // Modes and Models keep no rail row; every destination stays in the palette.
    expect(paletteLabels.map((label) => label.trim()).sort()).toEqual([
      "Capture",
      "Library",
      "Meetings",
      "Models",
      "Modes",
      "People",
      "Settings",
    ]);
    expect(paletteLabels).not.toContain("Overview");
    expect(paletteLabels).not.toContain("History");
  });

  test("a destination navigates and closes the palette", async ({ page }) => {
    await openApp(page);
    await page.keyboard.press("Meta+k");

    await page.getByRole("option", { name: "Modes", exact: true }).click();

    await expect(palette(page)).toHaveCount(0);
    /* Modes is a railless destination: the pane changes, and no rail button
     * claims the page. */
    await expect(page.getByRole("list", { name: "Your modes" })).toBeVisible();
    await expect(sidebarNav(page).locator('[aria-current="page"]')).toHaveCount(
      0,
    );
  });

  /* The keyboard half of the same contract: typing narrows to one destination,
   * which cmdk selects, and Enter is the press. */
  test("Enter on the one matched destination navigates too", async ({
    page,
  }) => {
    await openApp(page);
    await page.keyboard.press("Meta+k");

    await page.getByRole("combobox").fill("Settings");
    const option = page.getByRole("option", { name: "Settings", exact: true });
    await expect(option).toHaveAttribute("data-selected", "true");
    await page.keyboard.press("Enter");

    await expect(palette(page)).toHaveCount(0);
    await expect(
      sidebarNav(page).getByRole("button", { name: "Settings", exact: true }),
    ).toHaveAttribute("aria-current", "page");
  });

  /* ⌘K's second half, which nothing covered until a live run reported it
   * broken. Two characters in, the field is a search of the corpus, and the
   * plane's page becomes one titled section per kind.
   *
   * "notes" is the word the live corpus actually answered — it returned a
   * Meetings section — so it is the word typed here, and the empty-corpus case
   * below keeps the "sync" that reported the finding. Two readings of one
   * surface: a page of rows becomes sections, and no page becomes a sentence. */
  test("a page from the query plane becomes one section per kind", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: { sona_query_search: SEARCH_PAGE },
    });
    await page.goto("/");
    await expect(
      sidebarNav(page).getByRole("button", { name: "Capture", exact: true }),
    ).toBeVisible();
    await page.keyboard.press("Meta+k");
    await page.getByRole("combobox").fill("notes");

    for (const heading of ["Meetings", "People", "Dictations", "Open loops"]) {
      await expect(page.getByRole("group", { name: heading })).toBeVisible();
    }
    await expect(
      page.getByRole("option", { name: /Weekly planning/ }),
    ).toBeVisible();
    /* A row that matched semantically shares no letter with what was typed, so
       the list may reorder the plane's rows but may never filter one away. */
    await expect(page.getByText("Nothing matched")).toHaveCount(0);
  });

  /* The live report: typing a real word rendered the Ask row and nothing else,
   * which reads exactly like a broken search. It was not — the corpus had no
   * match — but the palette had no way to say so: `CommandEmpty` is cmdk's
   * "no rows at all" branch, and the Ask row is a row, so on a paired install
   * that branch can never fire. */
  test("a corpus that matched nothing says so beside the ask row", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        get_settings: PAIRED_SETTINGS,
        get_app_settings: PAIRED_SETTINGS,
      },
    });
    await page.goto("/");
    await expect(
      sidebarNav(page).getByRole("button", { name: "Capture", exact: true }),
    ).toBeVisible();
    await page.keyboard.press("Meta+k");
    await page.getByRole("combobox").fill("sync");

    // The row that used to be the only thing on screen.
    await expect(page.getByRole("option", { name: /Ask Sona/ })).toBeVisible();
    await expect(page.getByText("Nothing matched “sync”.")).toBeVisible();
  });
});

/* The palette's only animation, and now the app's only popup animation: the
 * shared `.popup-motion` shape from styles/primitives.css, which every
 * floating surface wears so a menu, a modal and a toast all arrive the same
 * way. What is asserted is that the palette is on it — 180ms of
 * `--duration-standard` under the `popup-enter` name — rather than on the
 * kit's own `enter` keyframe at the 150ms this surface used to override it
 * to. */
test.describe("the palette's motion", () => {
  const entrance = (page: Page) =>
    palette(page).evaluate((element) => {
      const style = getComputedStyle(element);
      return { duration: style.animationDuration, name: style.animationName };
    });

  test("enters on the shared 180ms popup shape", async ({ page }) => {
    await openApp(page);

    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toBeVisible();

    expect(await entrance(page)).toEqual({
      duration: "0.18s",
      name: "popup-enter",
    });
  });

  test("a device that asked to reduce motion gets no travel", async ({
    page,
  }) => {
    await installTauriMock(page);
    /* `emulateMedia`, not the `reducedMotion` fixture option: the fixture did
       not reach `window.matchMedia` in this harness, which would have made the
       assertions below pass for the wrong reason. */
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto("/");
    await expect(
      sidebarNav(page).getByRole("button", { name: "Capture", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => window.matchMedia("(prefers-reduced-motion: reduce)").matches,
      ),
    ).toBe(true);

    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toBeVisible();

    /* Two halves of the one promise. App.css collapses every CSS animation to
       0.01ms for this device, so the palette is at rest on the first frame and
       carries no transform; and the shape it collapses is the fade-only
       redefinition of `popup-enter`, so what is dropped is the movement rather
       than the arrival. The second half is read off the stylesheet because a
       0.01ms animation is over before `getAnimations` can be asked. */
    const { duration, name } = await entrance(page);
    expect(name).toBe("popup-enter");
    expect(Number.parseFloat(duration)).toBeLessThanOrEqual(0.001);
    await expect
      .poll(async () =>
        palette(page).evaluate((el) => getComputedStyle(el).transform),
      )
      .toBe("none");

    expect(
      await page.evaluate(() => {
        /* Every @keyframes popup-enter in the cascade, in order, keeping only
           the ones whose enclosing @media currently matches. The last of those
           is the definition in force. */
        const winning: string[][] = [];
        const walk = (rules: CSSRuleList, live: boolean): void => {
          for (const rule of Array.from(rules)) {
            if (rule instanceof CSSMediaRule) {
              walk(
                rule.cssRules,
                live && window.matchMedia(rule.conditionText).matches,
              );
            } else if (
              live &&
              rule instanceof CSSKeyframesRule &&
              rule.name === "popup-enter"
            ) {
              winning.push(
                Array.from(rule.cssRules).flatMap((frame) =>
                  // SAFETY: every child of a CSSKeyframesRule is a
                  // CSSKeyframeRule, whose `style` enumerates its properties.
                  Array.from((frame as CSSKeyframeRule).style),
                ),
              );
            }
          }
        };
        for (const sheet of Array.from(document.styleSheets)) {
          try {
            walk(sheet.cssRules, true);
          } catch {
            /* A cross-origin sheet cannot be read and holds none of ours. */
          }
        }
        return winning[winning.length - 1] ?? [];
      }),
    ).toEqual(["opacity", "opacity"]);
  });
});

/* The fold.
 *
 * The window is hard-locked at 900x800 (src-tauri/src/lib.rs), so "below the
 * fold" is one fixed number rather than a guess about somebody's monitor: it is
 * whatever the shell's one scroll region cannot show at rest.
 *
 * Capture is the default route and the page that has to answer at a glance.
 * Round 7 folded its two lists away behind closed summaries, so what the route
 * draws at rest is the hero, the row that needs an answer and two lines of
 * numbers - and the whole page fitting is a promise this route can now keep.
 * It could not before: the shipped build put a growing feed under a chart band
 * and cut the charts off at the bottom edge while this suite stayed green,
 * because the fixture behind it held a single feed row. Every other route may
 * scroll, because Library, Meetings and Settings are logs and a log that runs
 * past the window is still a log. What no route may do is put a section where
 * scrolling never reaches it. */
test.describe("the fold at the shipped window size", () => {
  test.use({ viewport: { width: 900, height: 800 } });

  const LOADING = "Loading…";
  const DESTINATIONS = [
    "Capture",
    "Library",
    "Modes",
    "Meetings",
    "People",
    "Settings",
    "Models",
  ] as const;

  interface FoldSection {
    name: string;
    top: number;
    bottom: number;
  }
  interface FoldReport {
    /** What the window shows of the scroll region, in CSS pixels. */
    visible: number;
    /** What scrolling covers. Equal to `visible` when nothing scrolls. */
    content: number;
    /**
     * What the route actually draws: its column's own height, padding
     * included. Read separately from `content` because Capture's column is
     * `min-h-full` and centred, so its box is the window's height whether the
     * page fills it or not — the number that answers "does this fit" is this
     * one.
     */
    natural: number;
    /** Every named section, offset from the top of the scrollable content. */
    sections: FoldSection[];
  }

  /* `main` holds exactly one child and that child is the region every page
   * scrolls inside (App.tsx), so this needs no class and no test id. The
   * sections are the pages' own `region` landmarks, which is what a reader
   * loses a whole one of when a route hides content. */
  const measureFold = (page: Page): Promise<FoldReport> =>
    page.getByRole("main").evaluate((main) => {
      // The pane's drag band sits before the scroll owner in `main`, so the
      // region is addressed by its slot rather than by position.
      // SAFETY: the slot is App.tsx's rendered <div>; evaluate types the tree
      // as bare Element, so the narrow restores what the DOM guarantees.
      const region = main.querySelector(
        '[data-slot="page-scroll"]',
      ) as HTMLElement;
      const origin = region.getBoundingClientRect().top - region.scrollTop;
      const nameOf = (node: HTMLElement): string => {
        const label = node.getAttribute("aria-label");
        if (label !== null) return label;
        const ids = node.getAttribute("aria-labelledby");
        if (ids === null) return "(unnamed)";
        return ids
          .split(/\s+/)
          .map((id) => document.getElementById(id)?.textContent?.trim() ?? "")
          .join(" ")
          .trim();
      };

      /* The route's own column is the last child of the region that draws
       * anything: the shell's banner column sits before it and collapses to
       * nothing on the ordinary path.
       *
       * Its height is measured first-drawn-child-top to last-drawn-child-bottom
       * rather than off its own box, because Capture's column is `min-h-full`
       * and centred — its box is the window's height whether the page fills it
       * or not. Gaps fall inside that span, the column's own padding is added
       * back around it, and its box height is the ceiling. Hidden children are
       * skipped at both levels: the collapsed banner here, and the settings
       * hub's inactive tab panel, which reports an all-zero box. */
      // SAFETY: children of a rendered element are HTMLElements here; the
      // browser-context Element typing is the only thing being widened past.
      const drawn = (Array.from(region.children) as HTMLElement[]).filter(
        (child) => child.getClientRects().length > 0,
      );
      const column = drawn[drawn.length - 1];
      const children =
        // SAFETY: same Element-to-HTMLElement restoration as `drawn` above.
        (Array.from(column?.children ?? []) as HTMLElement[]).filter(
          (child) => child.getClientRects().length > 0,
        );
      const first = children[0]?.getBoundingClientRect();
      const last = children[children.length - 1]?.getBoundingClientRect();
      const box = column?.getBoundingClientRect().height ?? 0;
      const edges = column === undefined ? null : getComputedStyle(column);
      const span =
        first === undefined || last === undefined || edges === null
          ? box
          : last.bottom -
            first.top +
            Number.parseFloat(edges.paddingTop) +
            Number.parseFloat(edges.paddingBottom);

      return {
        visible: region.clientHeight,
        content: region.scrollHeight,
        natural: Math.round(Math.min(box, span)),
        sections: Array.from(
          region.querySelectorAll<HTMLElement>(
            'section[aria-label], section[aria-labelledby], [role="region"]',
          ),
        ).map((section) => {
          const box = section.getBoundingClientRect();
          return {
            name: nameOf(section),
            top: Math.round(box.top - origin),
            bottom: Math.round(box.bottom - origin),
          };
        }),
      };
    });

  const openDestination = async (page: Page, name: string) => {
    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toBeVisible();
    await page.getByRole("option", { name, exact: true }).click();
    await expect(palette(page)).toHaveCount(0);
    // The route's chunk has landed: the Suspense skeleton announces itself,
    // and so does any page that waits on a command of its own.
    await expect(page.getByRole("status", { name: LOADING })).toHaveCount(0);
  };

  /* The page the app opens on, against the corpus that draws it at its
   * tallest. Two numbers, because either one alone passes on its own: the
   * column can draw less than the window shows while a child still hangs out
   * of it, and the scroll region can come up empty while the column is short
   * for a different reason. A fourth card, a taller hero or a summary that
   * ships open fails this. */
  test("Capture fits the window at rest, with nothing below the fold", async ({
    page,
  }) => {
    await installTauriMock(page, { responses: CAPTURE_AT_FULL_HEIGHT });
    await page.goto("/");
    await expect(
      page.getByRole("heading", { name: "Ready", exact: true }),
    ).toBeVisible();

    const report = await measureFold(page);
    /* Recorded on the run so the numbers behind these thresholds stay readable
     * without re-deriving them by hand. */
    test.info().annotations.push({
      type: "fold",
      description: `Capture: draws ${report.natural}px, window shows ${report.visible}px, scrolls ${report.content}px, across ${report.sections.length} named sections`,
    });

    // The measurement has to have measured a page: the route's own landmarks.
    expect(report.sections.length).toBeGreaterThan(0);
    // What the route draws is inside what the window shows, and the scroll
    // owner has nothing left over - so no card is cut by the bottom edge.
    expect(report.natural).toBeLessThanOrEqual(report.visible);
    expect(report.content).toBe(report.visible);
  });

  /* The same page, read once more with the chat column open.
   *
   * The column is part of the layout rather than a strip over it, so opening it
   * narrows the page: at the locked 900px the rail collapses to its glyph strip
   * and the content keeps what the column leaves. A window this size cannot
   * scroll sideways out of a column that does not fit — there is no wider
   * monitor to fall back on and no way to drag the window bigger — and it will
   * not show a scrollbar either, because the shell's row is `overflow-hidden`
   * all the way down. A column that overlaps the page and a column clipped off
   * the window's edge both look identical to `scrollWidth`, which is why the
   * scroll is only half of what this reads.
   *
   * The other half is that the three columns are three columns: the chat and
   * the page are horizontally disjoint, and both are inside the window. That is
   * this suite's own subject — content nobody can reach, which is what the rest
   * of the fold measures vertically — and it pins nothing the chat owns: not
   * its width, not which edge it opens against. Vertical fit is not re-asserted
   * here, because a narrower page reflows the feed and the feed is what this
   * route scrolls anyway. */
  test("Capture fits sideways with the chat column open", async ({ page }) => {
    await installTauriMock(page, {
      responses: {
        ...CAPTURE_AT_FULL_HEIGHT,
        get_settings: PAIRED_SETTINGS,
        get_app_settings: PAIRED_SETTINGS,
      },
    });
    await page.goto("/");
    await expect(page.getByRole("region", { name: "Ready" })).toBeVisible();

    /* The rail's chat row is the way in, and it needs the pairing above to be
     * live at all. It stays in the rail once the column is open — the column's
     * own close button owns the way back out — so it is pressed once and not
     * read again. */
    await page
      .getByRole("button", { name: "Chat with the Sona agent" })
      .click();
    const column = page.locator('[data-slot="chat-sheet"]');
    await expect(column).toBeVisible();
    /* The root travel gate clears from its offset transition's own
     * `transitionend`; the sheet is only a consumer of that clock. */
    const shell = page.locator(".app-shell");
    await expect(shell).toHaveAttribute("data-shell-moving", "true");
    await expect(shell).not.toHaveAttribute("data-shell-moving", "true");
    await page.evaluate(
      () =>
        new Promise<void>((resolve) => requestAnimationFrame(() => resolve())),
    );

    const sideways = await page.evaluate(() => {
      const spanOf = (selector: string) => {
        const node = document.querySelector(selector);
        if (node === null) throw new Error(`no ${selector} on this page`);
        const box = node.getBoundingClientRect();
        return { left: Math.round(box.left), right: Math.round(box.right) };
      };
      // SAFETY: the slot is App.tsx's rendered scroll owner, which is on the
      // page by the time the hero section inside it is visible.
      const pane = document.querySelector(
        '[data-slot="page-scroll"]',
      ) as HTMLElement;
      return {
        windowShows: document.documentElement.clientWidth,
        windowDraws: document.documentElement.scrollWidth,
        paneDraws: pane.scrollWidth,
        paneShows: pane.clientWidth,
        page: spanOf('[data-slot="page-scroll"]'),
        chat: spanOf('[data-slot="chat-sheet"]'),
      };
    });
    test.info().annotations.push({
      type: "fold",
      description: `Capture with the chat column open: window ${sideways.windowShows}px, page ${sideways.page.left}–${sideways.page.right}px, chat ${sideways.chat.left}–${sideways.chat.right}px, page draws ${sideways.paneDraws}px across ${sideways.paneShows}px`,
    });

    // Nothing sideways to scroll, on the window or inside the page.
    expect(sideways.windowDraws).toBeLessThanOrEqual(sideways.windowShows);
    expect(sideways.paneDraws).toBeLessThanOrEqual(sideways.paneShows);
    // Both columns inside the window, since a clipped one leaves no scroll.
    expect(sideways.chat.left).toBeGreaterThanOrEqual(0);
    expect(sideways.chat.right).toBeLessThanOrEqual(sideways.windowShows);
    expect(sideways.page.left).toBeGreaterThanOrEqual(0);
    expect(sideways.page.right).toBeLessThanOrEqual(sideways.windowShows);
    /* And they are beside each other rather than one on top of the other: the
     * page under an opaque column is the unreachable content this whole suite
     * exists to catch. Either order passes; the chat picks its own edge. */
    const disjoint =
      sideways.page.right <= sideways.chat.left ||
      sideways.chat.right <= sideways.page.left;
    expect(
      disjoint,
      `the chat column (${sideways.chat.left}–${sideways.chat.right}px) overlaps the page (${sideways.page.left}–${sideways.page.right}px)`,
    ).toBe(true);
  });

  test("no route puts a section where scrolling never reaches it", async ({
    page,
  }) => {
    await installTauriMock(page, { responses: CAPTURE_AT_FULL_HEIGHT });
    await page.goto("/");
    await expect(
      sidebarNav(page).getByRole("button", { name: "Capture", exact: true }),
    ).toBeVisible();

    for (const destination of DESTINATIONS) {
      await openDestination(page, destination);
      const report = await measureFold(page);
      test.info().annotations.push({
        type: "fold",
        description: `${destination}: draws ${report.natural}px, window shows ${report.visible}px, scrolls ${report.content}px`,
      });

      for (const section of report.sections) {
        expect(
          section.top,
          `${destination}: "${section.name}" starts above the scroll region`,
        ).toBeGreaterThanOrEqual(0);
        expect(
          section.bottom,
          `${destination}: "${section.name}" ends past what scrolling reaches`,
        ).toBeLessThanOrEqual(report.content);
      }
    }
  });
});

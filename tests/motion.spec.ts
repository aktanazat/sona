import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { MODES_SNAPSHOT } from "./support/tauri-fixtures";
import { installTauriMock, type JsonValue } from "./support/tauri-mock";

/* The Motion conversions that are left, in a real browser with a real frame
 * loop.
 *
 * Everything provable without a DOM is proved in the unit tests; what needs
 * Chromium is the part that only exists on screen — that a pointer drag ends
 * in the backend command carrying the order it produced.
 *
 * The command palette and the segmented tab strips used to be proved here
 * too. The palette runs on cmdk and a CSS transition now — its specs,
 * including reduced motion and the auto-repeat flicker regression, live in
 * shell.spec.ts — and every tablist in the app is a vg/Radix strip with no
 * Motion mark, so there is no moving indicator left to sample. */

const MODE_IDS = MODES_SNAPSHOT.modes.map((mode) => mode.id);

/** MODES_SNAPSHOT reordered, which is what the backend answers a reorder with. */
const reorderedSnapshot = (orderedIds: readonly string[]) => ({
  ...MODES_SNAPSHOT,
  modes: orderedIds.map((id) => {
    const mode = MODES_SNAPSHOT.modes.find((candidate) => candidate.id === id);
    if (!mode) throw new Error(`no fixture mode ${id}`);
    return mode;
  }),
  revision: MODES_SNAPSHOT.revision + 1,
});

/* Command arguments cross the Tauri bridge as JSON, so JSON is their exact
 * value contract — the same one `tauri-mock` states for its responses. */
type CommandArgs = Record<string, JsonValue>;

interface Invocation {
  command: string;
  args: CommandArgs;
}

/**
 * Records every command the app invokes, with its arguments.
 *
 * Registered after the mock so it wraps the mock's `invoke` instead of being
 * overwritten by it: `addInitScript` runs in registration order.
 */
async function recordInvocations(page: Page): Promise<void> {
  await page.addInitScript(() => {
    // SAFETY: the Tauri globals are planted by the mock; Window does not
    // declare them, so an assertion is the only way to reach one.
    const tauri = window as Window & {
      __TAURI_INTERNALS__: {
        invoke: (command: string, args?: CommandArgs) => Promise<JsonValue>;
      };
      __invocations?: Invocation[];
    };
    const calls: Invocation[] = [];
    tauri.__invocations = calls;
    const inner = tauri.__TAURI_INTERNALS__.invoke;
    tauri.__TAURI_INTERNALS__.invoke = (command, args) => {
      calls.push({ command, args: args ?? {} });
      return inner(command, args);
    };
  });
}

const invocations = (page: Page): Promise<Invocation[]> =>
  page.evaluate(() => {
    // SAFETY: planted by recordInvocations in the same page.
    const recorded = window as Window & { __invocations?: Invocation[] };
    return recorded.__invocations ?? [];
  });

/**
 * Opens the app on the Modes page.
 *
 * Modes has no rail row any more — picking a mode is a Capture decision and
 * editing one is a Settings task — so the destination is reached the way every
 * railless destination is: through the ⌘K palette, which lists all of them.
 */
const openModesPage = async (page: Page) => {
  await page.goto("/");
  /* The ⌘K listener mounts with the app shell; pressing before the rail
   * renders races it and the palette never opens. */
  await expect(
    page.getByRole("navigation", { name: "Main navigation" }),
  ).toBeVisible();
  await page.keyboard.press("Meta+k");
  const destination = page.getByRole("option", { name: "Modes", exact: true });
  await expect(destination).toBeVisible();
  await destination.click();
};

test.describe("mode list reorder", () => {
  const swapped = [MODE_IDS[1], MODE_IDS[0], ...MODE_IDS.slice(2)];

  /**
   * Drags the second row up past the first, in steps, and holds.
   *
   * `hover()` rather than a computed `mouse.move`: navigating to Modes runs a
   * view transition, and while one is playing the real DOM is replaced by
   * snapshot pseudo-elements, so a raw pointer press at the row's coordinates
   * lands on `<html>` and no gesture ever starts. Playwright's actionability
   * wait is what makes the press hit the row.
   */
  const dragSecondRowUp = async (page: Page) => {
    const row = modeRows(page).nth(1);
    await row.hover();
    const box = (await row.boundingBox())!;
    const x = box.x + box.width / 2;
    const y = box.y + box.height / 2;
    await page.mouse.down();
    /* Past the halfway point of the row above, in steps, so Motion sees a real
       gesture with a velocity rather than one teleport. */
    for (let step = 1; step <= 8; step += 1) {
      await page.mouse.move(x, y - (box.height * step) / 8);
    }
  };

  /* The visible "Your modes" heading is gone — the page title and the view tab
     both already say Modes — so the list is reached by its accessible name. */
  const modeRows = (page: Page) =>
    page.getByRole("list", { name: "Your modes" }).getByRole("listitem");

  const openModes = async (page: Page) => {
    await openModesPage(page);
    const rows = modeRows(page);
    await expect(rows).toHaveCount(MODE_IDS.length);
    /* The draggable list is an async chunk, and this attribute only exists once
       it has landed — so waiting on it also proves the lazy boundary resolves
       and that the plain fallback was replaced. */
    await expect(rows.first()).toHaveAttribute("data-reorderable", "true");
    return rows;
  };

  test("dragging a row commits the new order to the backend", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: { reorder_modes: reorderedSnapshot(swapped) },
    });
    await recordInvocations(page);
    await openModes(page);

    await dragSecondRowUp(page);
    await page.mouse.up();

    await expect
      .poll(async () =>
        (await invocations(page)).some(
          (call) => call.command === "reorder_modes",
        ),
      )
      .toBe(true);

    const call = (await invocations(page)).find(
      (entry) => entry.command === "reorder_modes",
    );
    expect(call?.args.orderedIds).toEqual(swapped);
    expect(call?.args.expectedRevision).toBe(MODES_SNAPSHOT.revision);
  });

  test("the committed order is the order the list then shows", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: { reorder_modes: reorderedSnapshot(swapped) },
    });
    await openModes(page);

    await dragSecondRowUp(page);
    await page.mouse.up();

    await expect
      .poll(async () =>
        (
          await modeRows(page).first().locator("span").first().textContent()
        )?.trim(),
      )
      .toBe(MODES_SNAPSHOT.modes[1].name);
  });

  test("the keyboard route commits the same full order", async ({ page }) => {
    await installTauriMock(page, {
      responses: { reorder_modes: reorderedSnapshot(swapped) },
    });
    await recordInvocations(page);
    const rows = await openModes(page);

    /* The overflow menu is a Radix dropdown now: its trigger is a button
       named for the mode it acts on, and its items only exist once open. */
    await rows
      .nth(1)
      .getByRole("button", { name: /^actions for/i })
      .click();
    await page
      .getByRole("menuitem", { name: /move up/i })
      .first()
      .click();

    await expect
      .poll(
        async () =>
          (await invocations(page)).find(
            (entry) => entry.command === "reorder_modes",
          )?.args.orderedIds,
      )
      .toEqual(swapped);
  });

});

import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { MEETING_REVIEW } from "./support/tauri-fixtures";
import { installTauriMock } from "./support/tauri-mock";

/* One finished meeting, opened to its review page: where the two keyboard
 * tests at the bottom of this file start. */
const openReview = async (page: Page) => {
  await installTauriMock(page, {
    responses: {
      meeting_list: {
        entries: [
          {
            kind: "meeting",
            session_id: "meeting-1",
            title: "Pricing review with Northwind",
            phase: "review_ready",
            created_at_utc_ms: 1_756_136_400_000,
            capture_completeness: "partial",
            processing_status: { kind: "succeeded" },
            recorded_duration_ms: 1_800_000,
          },
        ],
        has_more: false,
      },
      meeting_get: MEETING_REVIEW,
    },
  });

  await page.goto("/");
  await page.getByRole("button", { name: "Meetings", exact: true }).click();
  await page
    .getByRole("button", { name: /Pricing review with Northwind/ })
    .click();
};

test.describe("Meetings", () => {
  test("one press records, under a disclosure that is on screen first", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        // One pending suggestion, so the meetings surface renders the detected
        // meeting path alongside its own start block.
        meeting_suggestions_list: [
          {
            offer_id: "offer-1",
            provider: "zoom",
            app_bundle_id: "us.zoom.xos",
            evidence_flags: {
              appOnly: true,
              axTitle: false,
              axHost: false,
              axUnavailable: false,
            },
            observed_at_ns: 1,
            expires_at_ns: 2,
          },
        ],
      },
    });

    await page.goto("/");
    // Meetings is a first-class sidebar destination: the sidebar shell
    // promoted it out of the old Library sub-nav, and the row lands on the
    // same meetings surface the deep-link handler targets.
    await page.getByRole("button", { name: "Meetings", exact: true }).click();

    const start = page
      .getByRole("button", { name: "Record", exact: true })
      .first();
    // The promise the press makes has to be readable before the press:
    // pressing Record is what the backend records as the acknowledgment, and
    // round 7 moved the sentence out of a card and onto the title line, where
    // it is the one Meta line under the heading.
    await expect(
      page
        .getByText("Records this Mac's audio locally. Nothing joins the call.")
        .first(),
    ).toBeVisible();
    await expect(start).toBeEnabled();
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem("meeting-started"))),
      )
      .toBe(0);

    // No setup screen in between: this press creates the session and starts
    // capture in one action.
    await start.click();

    // Stop exists only while capture is running, and only on the live surface,
    // so it is the state itself rather than a word describing the state.
    await expect(
      page.getByRole("button", { name: "Stop", exact: true }),
    ).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem("meeting-started"))),
      )
      .toBe(1);
  });

  test("a meeting an interrupted launch left behind says why and offers to run it again", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        // The shape yesterday's launch leaves on disk once startup recovery has
        // resolved it: parked for a person, with a terminal reason.
        meeting_list: {
          entries: [
            {
              kind: "meeting",
              session_id: "meeting-stranded",
              title: "Yesterday's standup",
              phase: "recovery_required",
              created_at_utc_ms: 1_756_136_400_000,
              capture_completeness: "partial",
              processing_status: { kind: "failed", reason: "interrupted" },
              recorded_duration_ms: 1_800_000,
            },
          ],
          has_more: false,
        },
        // Empty, so the only control offered for this meeting is the one on its
        // own row: the recovery card at the top of the page is not on screen.
        meeting_recovery_list: [],
      },
    });

    await page.goto("/");
    await page.getByRole("button", { name: "Meetings", exact: true }).click();

    const row = page.getByRole("listitem").filter({
      hasText: "Yesterday's standup",
    });
    // One word per row, and it is the reason: "Needs attention" is the state,
    // and a row that can say why does not say both.
    await expect(row.getByText("Needs attention")).toHaveCount(0);
    // The state alone does not say what happened, so the reason is on the row.
    await expect(row.getByText("Interrupted before it finished")).toBeVisible();
    await expect(
      row.getByRole("button", { name: "Try again", exact: true }),
    ).toBeEnabled();
    // The bug this replaced: a meeting nothing was working on reading as work
    // in flight, with no way to act on it.
    await expect(row.getByText("Processing", { exact: true })).toHaveCount(0);
  });

  test("renaming from the menu hands the keyboard to the title field", async ({
    page,
  }) => {
    await openReview(page);
    await page.getByRole("button", { name: "More", exact: true }).click();
    await page.getByRole("menuitem", { name: "Rename", exact: true }).click();

    // The press that opens the field is the press that hands over the
    // keyboard, because renaming is typing: a reader who presses Rename and
    // types straight away has to see those keystrokes land in the title.
    const field = page.getByRole("textbox", { name: "Meeting title" });
    await expect(field).toBeFocused();
    await page.keyboard.type("Q3 ");
    await expect(field).toHaveValue(/Q3 /);
  });

  /* Rename is the one verb on that menu which takes the keyboard, so the
   * handler that hands it over has to leave every other way out alone: a menu
   * dismissed without a verb owes the reader the button they opened it with,
   * or the keyboard lands on the document and the page is gone. */
  test("dismissing the menu leaves the keyboard on the button that opened it", async ({
    page,
  }) => {
    await openReview(page);

    const more = page.getByRole("button", { name: "More", exact: true });
    await more.click();
    await page.keyboard.press("Escape");

    await expect(more).toBeFocused();
  });
});

import { expect, test } from "@playwright/test";

import { meetingStartedAtMs } from "./support/tauri-fixtures";
import { installTauriMock } from "./support/tauri-mock";

const CALENDAR_PROMPT = {
  eventSchemaVersion: 2,
  promptId: "prompt-1",
  prompt: {
    kind: "CalendarEvent",
    eventKey: "weekly-sync#1756136400000",
    eventTitle: "Weekly sync",
  },
  notificationTitle: "Weekly sync starting",
  delivery: "panel",
  showIntroduction: true,
};

const ACTIVE_PANEL_STATE = {
  snapshot: {
    session_id: "meeting-1",
    phase: "capturing_recording",
    revision: 2,
    title: "Existing recording",
    started_at_utc_ms: meetingStartedAtMs(),
  },
  standing_series_key: null,
  disclosure: { kind: "not_asked" },
};

const PREP_RITUAL = {
  eventSchemaVersion: 2,
  ritualId: "prep:weekly-product",
  ritual: {
    kind: "prep",
    card: {
      eventKey: "weekly-product#next",
      seriesKey: "weekly-product",
      title: "Weekly product review",
      startUtcMs: Date.now() + 5 * 60_000,
      lastMeetingId: "00000000-0000-0000-0000-000000000010",
      headline: "Launch sequencing stayed unresolved.",
      mineOpenLoops: ["Send the launch plan", "Confirm beta dates"],
      mineOpenLoopCount: 2,
      waitingOnCount: 1,
      participants: [
        { name: "Maya", meetingsCount: 6, organization: "Northstar" },
        { name: "Jon", meetingsCount: 2, organization: null },
      ],
      canRecordWhenStarts: true,
    },
  },
  notificationTitle: "Weekly product review — in 5 minutes",
  delivery: "panel",
};

const WRAP_RITUAL = {
  eventSchemaVersion: 2,
  ritualId: "wrap:00000000-0000-0000-0000-000000000020",
  ritual: {
    kind: "wrap",
    card: {
      sessionId: "00000000-0000-0000-0000-000000000020",
      title: "Weekly product review",
      headline: "The launch plan is ready for review.",
      followUpCount: 2,
      waitingOnCount: 1,
      waitingOnNames: ["Maya"],
    },
  },
  notificationTitle: "Weekly product review — saved",
  delivery: "panel",
};

test.describe("Consent panel", () => {
  test("acknowledges, records with the panel command, and stops from the pill", async ({
    page,
  }) => {
    await installTauriMock(page, {
      events: { "detection-prompt": [CALENDAR_PROMPT] },
    });

    await page.goto("/consent");

    await expect(
      page.getByRole("heading", { name: "Record Weekly sync?" }),
    ).toBeVisible();
    await expect(
      page.getByText("Sona records on this Mac and keeps you in control.", {
        exact: true,
      }),
    ).toBeVisible();
    const alwaysRecord = page.getByRole("checkbox", {
      name: "Always record this meeting",
    });
    await expect(alwaysRecord).toBeVisible();
    await alwaysRecord.click();
    await expect
      .poll(() =>
        page.evaluate(() => localStorage.getItem("detection-panel-ack")),
      )
      .toBe("prompt-1");
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem("meeting-started"))),
      )
      .toBe(0);

    await page.getByRole("button", { name: "Record", exact: true }).click();

    await expect(page.getByText("Recording", { exact: true })).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem("meeting-started"))),
      )
      .toBe(1);
    const startRequest = await page.evaluate(() =>
      JSON.parse(
        localStorage.getItem("meeting-consent-panel-start-request") ?? "null",
      ),
    );
    expect(startRequest).toMatchObject({
      prompt_id: "prompt-1",
      always_record_series: true,
      consent: {
        microphone_acknowledged: true,
        system_audio_acknowledged: true,
      },
    });
    expect(startRequest.operation_id).toEqual(expect.any(String));

    await page.getByRole("button", { name: "Stop", exact: true }).click();

    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem("meeting-stopped"))),
      )
      .toBe(1);
    await expect(page.getByText("Recording", { exact: true })).toBeHidden();
  });

  test("Ignore retracts the panel without starting a meeting", async ({
    page,
  }) => {
    await installTauriMock(page, {
      events: {
        "detection-prompt": [{ ...CALENDAR_PROMPT, promptId: "prompt-ignore" }],
      },
    });
    await page.goto("/consent");
    await expect(
      page.getByRole("heading", { name: "Record Weekly sync?" }),
    ).toBeVisible();

    await page.getByRole("button", { name: "Ignore", exact: true }).click();

    await expect(
      page.getByRole("heading", { name: "Record Weekly sync?" }),
    ).toBeHidden();
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem("meeting-started"))),
      )
      .toBe(0);
    await expect
      .poll(() =>
        page.evaluate(() =>
          JSON.parse(
            localStorage.getItem("detection-prompt-response") ?? "null",
          ),
        ),
      )
      .toEqual({ promptId: "prompt-ignore", accepted: false });
  });

  /* The prompt is a dialog the keyboard can drive: focus starts on Record,
   * Tab cycles inside the card, and Escape is Ignore. The card stays for its
   * exit fade rather than vanishing under the pointer. */
  test("the prompt is a dialog: Record takes focus, Tab cycles, Escape ignores", async ({
    page,
  }) => {
    await installTauriMock(page, {
      events: {
        "detection-prompt": [{ ...CALENDAR_PROMPT, promptId: "prompt-keys" }],
      },
    });
    await page.goto("/consent");

    const dialog = page.getByRole("dialog", { name: "Record Weekly sync?" });
    await expect(dialog).toBeVisible();
    await expect(dialog).toHaveAccessibleDescription(
      /Sona records on this Mac and keeps you in control\./,
    );
    const record = page.getByRole("button", { name: "Record", exact: true });
    await expect(record).toBeFocused();

    await page.keyboard.press("Tab");
    await expect(
      page.getByRole("checkbox", { name: "Always record this meeting" }),
    ).toBeFocused();
    await page.keyboard.press("Shift+Tab");
    await expect(record).toBeFocused();

    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect
      .poll(() =>
        page.evaluate(() =>
          JSON.parse(
            localStorage.getItem("detection-prompt-response") ?? "null",
          ),
        ),
      )
      .toEqual({ promptId: "prompt-keys", accepted: false });
  });

  test("Escape dismisses PREP and finishes WRAP, and leaves the pill alone", async ({
    page,
  }) => {
    await installTauriMock(page, {
      events: { "meeting-ritual": [PREP_RITUAL] },
    });
    await page.goto("/consent");
    await expect(
      page.getByRole("dialog", { name: /Weekly product review — in/ }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Record when it starts" }),
    ).toBeFocused();
    await page.keyboard.press("Escape");
    await expect
      .poll(() =>
        page.evaluate(() =>
          JSON.parse(localStorage.getItem("meeting-ritual-response") ?? "null"),
        ),
      )
      .toEqual({ ritualId: PREP_RITUAL.ritualId, action: "prep_dismiss" });

    await installTauriMock(page, {
      events: { "meeting-ritual": [WRAP_RITUAL] },
    });
    await page.goto("/consent");
    await expect(
      page.getByRole("dialog", { name: "Weekly product review — saved" }),
    ).toBeVisible();
    await expect(page.getByRole("button", { name: "Done" })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect
      .poll(() =>
        page.evaluate(() =>
          JSON.parse(localStorage.getItem("meeting-ritual-response") ?? "null"),
        ),
      )
      .toEqual({ ritualId: WRAP_RITUAL.ritualId, action: "wrap_done" });

    await installTauriMock(page, {
      responses: { meeting_consent_panel_active_state: ACTIVE_PANEL_STATE },
    });
    await page.goto("/consent");
    await expect(page.getByText("Recording", { exact: true })).toBeVisible();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await page.keyboard.press("Escape");
    await expect(page.getByText("Recording", { exact: true })).toBeVisible();
    expect(
      await page.evaluate(() =>
        Number(localStorage.getItem("meeting-stopped")),
      ),
    ).toBe(0);
  });

  test("retraction clears the matching panel prompt", async ({ page }) => {
    await installTauriMock(page, {
      events: {
        "detection-prompt": [
          { ...CALENDAR_PROMPT, promptId: "prompt-retracted" },
        ],
        "detection-prompt-retracted": [
          {
            eventSchemaVersion: 2,
            promptId: "prompt-retracted",
            reason: "event_ended",
          },
        ],
      },
    });

    await page.goto("/consent");

    await expect
      .poll(() =>
        page.evaluate(() => localStorage.getItem("detection-panel-ack")),
      )
      .toBe("prompt-retracted");
    await expect(page.getByRole("dialog")).toBeHidden();
  });

  test("does not acknowledge a prompt hidden by an active recording", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        meeting_consent_panel_active_state: ACTIVE_PANEL_STATE,
      },
      events: {
        "detection-prompt": [
          { ...CALENDAR_PROMPT, promptId: "prompt-behind-recording" },
        ],
      },
    });

    await page.goto("/consent");

    await expect(page.getByText("Recording", { exact: true })).toBeVisible();
    await expect(page.getByRole("dialog")).toBeHidden();
    await expect
      .poll(() =>
        page.evaluate(() => localStorage.getItem("detection-panel-ack")),
      )
      .toBe("");
  });

  test("shows the recurring-meeting PREP context and opens its brief", async ({
    page,
  }) => {
    await installTauriMock(page, {
      events: { "meeting-ritual": [PREP_RITUAL] },
    });
    await page.goto("/consent");

    await expect(page.getByTestId("prep-card")).toBeVisible();
    await expect(
      page.getByRole("heading", {
        name: /Weekly product review — in/,
      }),
    ).toBeVisible();
    await expect(
      page.getByText("Last time: Launch sequencing stayed unresolved."),
    ).toBeVisible();
    await expect(page.getByText("Send the launch plan")).toBeVisible();
    await expect(page.getByText("Confirm beta dates")).toBeVisible();
    await expect(page.getByText(/Maya · 6 meetings · Northstar/)).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Record when it starts" }),
    ).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(() => localStorage.getItem("meeting-ritual-panel-ack")),
      )
      .toBe(PREP_RITUAL.ritualId);

    await page.getByRole("button", { name: "Open brief" }).click();
    await expect
      .poll(() =>
        page.evaluate(() =>
          JSON.parse(localStorage.getItem("meeting-ritual-response") ?? "null"),
        ),
      )
      .toEqual({
        ritualId: PREP_RITUAL.ritualId,
        action: "prep_open_brief",
      });
  });

  test("shows WRAP deltas and keeps a stable copied confirmation", async ({
    page,
  }) => {
    await page
      .context()
      .grantPermissions(["clipboard-read", "clipboard-write"]);
    await installTauriMock(page, {
      events: { "meeting-ritual": [WRAP_RITUAL] },
    });
    await page.goto("/consent");

    await expect(page.getByTestId("wrap-card")).toBeVisible();
    await expect(
      page.getByRole("heading", {
        name: "Weekly product review — saved",
      }),
    ).toBeVisible();
    await expect(
      page.getByText("The launch plan is ready for review."),
    ).toBeVisible();
    await expect(
      page.getByText(/2 follow-ups · 1 waiting on Maya/),
    ).toBeVisible();

    await page.getByRole("button", { name: "Copy follow-up" }).click();
    await expect(page.getByRole("button", { name: "Copied" })).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(() =>
          JSON.parse(localStorage.getItem("meeting-ritual-response") ?? "null"),
        ),
      )
      .toEqual({
        ritualId: WRAP_RITUAL.ritualId,
        action: "wrap_follow_up_copied",
      });

    await page.getByRole("button", { name: "Done" }).click();
    await expect(page.getByTestId("wrap-card")).toBeHidden();
  });
});

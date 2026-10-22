import { describe, expect, test } from "bun:test";
import { createInstance, type TFunction } from "i18next";
import { followUpDraftText, type MeetingFollowUpDraft } from "./followUpDraft";

/* The fallback path is the one worth pinning: it is what a machine without a
 * meeting-intelligence engine actually shows, and it is the reason the button
 * is never dead. */

/* A real i18next instance over a flat catalog. The draft takes i18next's own
 * `t`, so handing it one is cheaper than describing one: a stand-in function
 * has to be asserted into the shape of `TFunction`, and that assertion pins
 * the test's guess at the shape rather than anything about the draft. */
const translator = (catalog: Record<string, string>): TFunction => {
  const instance = createInstance();
  void instance.init({
    lng: "en",
    keySeparator: false,
    resources: { en: { translation: catalog } },
  });
  return instance.t;
};

const t = translator({
  "meetings.followUp.iOwe": "What I owe",
  "meetings.followUp.decisions": "What we decided",
});

const draft = (
  overrides: Partial<MeetingFollowUpDraft>,
): MeetingFollowUpDraft => ({
  session_id: "session",
  title: "Pricing review",
  source: "structured",
  message: null,
  summary: "Pricing stayed open and Dana took the comparison.",
  mine: ["Send the tier comparison"],
  decisions: ["Ship on Tuesday"],
  receipt: {
    schema_version: 1,
    operation_id: "operation",
    session_id: "session",
    actor: "user",
    command: "follow_up_draft",
    expected_revision: 0,
    from_phase: "review_ready",
    to_phase: "review_ready",
    requested_at_utc_ms: 0,
    committed_at_utc_ms: 0,
    result: "committed",
    reason_codes: [],
    new_revision: 0,
    effect_ids: ["structured-fallback"],
  },
  ...overrides,
});

describe("followUpDraftText", () => {
  test("an engine's message is the draft, untouched", () => {
    const text = followUpDraftText(
      draft({
        source: "generated",
        message: "Thanks all — I'll send the tier comparison this week.",
      }),
      t,
    );

    expect(text).toBe("Thanks all — I'll send the tier comparison this week.");
  });

  test("without an engine the record itself is the draft", () => {
    expect(followUpDraftText(draft({}), t)).toBe(
      [
        "Pricing stayed open and Dana took the comparison.",
        "What I owe\n- Send the tier comparison",
        "What we decided\n- Ship on Tuesday",
      ].join("\n\n"),
    );
  });

  test("a section the meeting has nothing for is left out, not left empty", () => {
    expect(followUpDraftText(draft({ mine: [], decisions: [] }), t)).toBe(
      "Pricing stayed open and Dana took the comparison.",
    );
    expect(followUpDraftText(draft({ summary: "", decisions: [] }), t)).toBe(
      "What I owe\n- Send the tier comparison",
    );
  });
});

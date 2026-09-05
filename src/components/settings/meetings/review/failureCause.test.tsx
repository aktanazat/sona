import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type {
  AllowedMeetingAction,
  EngineFailureCause,
  MeetingReviewSnapshot,
  ProcessingStatus,
} from "@/bindings";
import { InsightsTab } from "./InsightsTab";

const localePath = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);
// SAFETY: the en bundle is repo-owned and check:translations pins these keys;
// the narrow states the shape this test reads, not a guess about foreign data.
const catalogue = JSON.parse(fs.readFileSync(localePath, "utf8")) as {
  meetings: {
    processing: { cause: Record<EngineFailureCause, string> };
    review: { regenerate: string; generationFailedDescription: string };
  };
};
const { cause } = catalogue.meetings.processing;
const { regenerate, generationFailedDescription } = catalogue.meetings.review;
const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: catalogue } },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});

/** The Regenerate link itself. The word also appears inside the sentence a
 * causeless row reads, so a test about the affordance has to name the
 * element and not the word. */
const RETRY_LINK = `>${regenerate}</button>`;

const SNAPSHOT: MeetingReviewSnapshot = {
  session: {
    session_id: "meeting-1",
    phase: "recovery_required",
    revision: 4,
    title: "Pricing review",
    started_at_utc_ms: 1_760_000_000_000,
    elapsed_offset_ns: 1_845_000_000_000,
    sources: [],
    open_capture_window_started_at_ns: null,
    capture_completeness: "complete",
    storage: "available",
    processing_status: { kind: "succeeded" },
    retention_deadline_utc_ms: null,
    allowed_actions: ["edit", "regenerate"],
  },
  tracks: [],
  gaps: [],
  speakers: [],
  transcript: [],
  notes: [],
  artifacts: [],
  questions: [],
  diarization: {
    status: "succeeded",
    model_id: "diarizer",
    model_version: "1",
    generation_id: "generation-1",
    assigned_segment_count: 0,
  },
  can_export: true,
  remote_cancellation_pending: false,
};

const failed = (cause: EngineFailureCause | null): ProcessingStatus => ({
  kind: "failed",
  reason: "engine_failure",
  cause,
});

/** The insights tab as one string, with the entities React escapes on the way
 * out turned back into the characters the catalogue writes. */
const insights = (
  status: ProcessingStatus,
  allowedActions: AllowedMeetingAction[] = ["edit", "regenerate"],
) =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <InsightsTab
        snapshot={{
          ...SNAPSHOT,
          session: {
            ...SNAPSHOT.session,
            processing_status: status,
            allowed_actions: allowedActions,
          },
        }}
        busy={false}
        editable={true}
        newNote=""
        analytics={null}
        speakerNames={{}}
        doneActionItems={new Set()}
        onNewNoteChange={() => {}}
        onCreateNote={() => {}}
        onNoteUpdate={() => {}}
        onNoteDelete={() => {}}
        onJumpToSegment={() => {}}
        onActionItemToggle={() => {}}
        onRefresh={() => Promise.resolve()}
        onAnalyticsRefresh={() => Promise.resolve()}
        onOpenSettings={() => {}}
        onRegenerate={() => {}}
      />
    </I18nextProvider>,
  )
    .replace(/&#x27;/g, "'")
    .replace(/&quot;/g, '"')
    .replace(/&amp;/g, "&");

describe("what a failed generation tells the reader", () => {
  test("a record that could not be read says so and offers no second run", () => {
    const markup = insights(failed("storage"));
    expect(markup).toContain(cause.storage);
    expect(markup).not.toContain(generationFailedDescription);
    expect(markup).not.toContain(RETRY_LINK);
  });

  test("a model that returned nothing usable offers the second run", () => {
    const markup = insights(failed("model_refused"));
    expect(markup).toContain(cause.model_refused);
    expect(markup).toContain(RETRY_LINK);
  });

  test("a meeting that no longer takes a regeneration is not offered one", () => {
    const markup = insights(failed("model_refused"), ["edit"]);
    expect(markup).toContain(cause.model_refused);
    expect(markup).not.toContain(RETRY_LINK);
  });

  test("a row stored before causes existed keeps the sentence it had", () => {
    const markup = insights(failed(null));
    expect(markup).toContain(generationFailedDescription);
    expect(markup).not.toContain(RETRY_LINK);
  });
});

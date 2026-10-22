import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type {
  CitedArtifactText,
  MeetingArtifactRevision,
  MeetingReviewSnapshot,
  SummaryLineTrace,
} from "@/bindings";
import { TracedSummary } from "./Citations";
import { MeetingArtifactPanel } from "./MeetingArtifactPanel";
import { summaryLines } from "./summaryTrace";
import { TranscriptTab } from "./TranscriptTab";

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
const catalogue = JSON.parse(fs.readFileSync(localePath, "utf8"));
const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: catalogue } },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});
const render = (node: React.ReactElement) =>
  renderToStaticMarkup(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);
const noop = () => undefined;

const FIRST_LINE = "Pricing stayed open.";
const SECOND_LINE = "Dana took the tier comparison.";

const summary = (text: string): CitedArtifactText => ({
  text,
  citations: [
    {
      segment_id: "segment-1",
      start_offset_ns: 12_000_000_000,
      end_offset_ns: 14_000_000_000,
    },
  ],
});

const TRACE: SummaryLineTrace[] = [
  {
    line: 0,
    anchor: {
      segment_id: "segment-1",
      start_offset_ns: 12_000_000_000,
      end_offset_ns: 14_000_000_000,
    },
  },
  {
    line: 1,
    anchor: {
      segment_id: "segment-2",
      start_offset_ns: 30_000_000_000,
      end_offset_ns: 31_500_000_000,
    },
  },
];

const BOTH_LINES = `${FIRST_LINE}\n${SECOND_LINE}`;

describe("a summary line is the way back to its moment", () => {
  test("every traced line is pressable and says which moment it opens", () => {
    const markup = render(
      <TracedSummary
        summary={summary(BOTH_LINES)}
        trace={TRACE}
        onJump={noop}
      />,
    );

    expect(markup).toContain(FIRST_LINE);
    expect(markup).toContain(SECOND_LINE);
    /* The timestamp is the only decoration a line gets: quiet, tabular, and
     * the same fact the aria label carries for a reader who cannot see it. */
    expect(markup).toContain("0:12");
    expect(markup).toContain("0:30");
    expect(markup).toContain('aria-label="Jump to the transcript at 0:12"');
    expect(markup).toContain('aria-label="Jump to the transcript at 0:30"');
  });

  test("a summary with no provenance reads as it always did", () => {
    const untraced = render(
      <TracedSummary summary={summary(BOTH_LINES)} trace={[]} onJump={noop} />,
    );
    const absent = render(
      <TracedSummary
        summary={summary(BOTH_LINES)}
        trace={undefined}
        onJump={noop}
      />,
    );

    for (const markup of [untraced, absent]) {
      expect(markup).toContain(FIRST_LINE);
      expect(markup).toContain(SECOND_LINE);
      /* The block citation stays: it is the only jump an untraced summary has
       * ever offered, and losing it would be a regression, not a fallback. */
      expect(markup).toContain("0:12");
    }
  });

  test("a line the generator did not trace stays plain beside ones it did", () => {
    const markup = render(
      <TracedSummary
        summary={summary(BOTH_LINES)}
        trace={[TRACE[1]]}
        onJump={noop}
      />,
    );

    expect(markup).toContain(FIRST_LINE);
    expect(markup).toContain(SECOND_LINE);
    expect(markup).toContain("0:30");
    expect(markup).not.toContain("0:12");
  });

  test("the panel hands the artifact's own map to its summary", () => {
    const artifact: MeetingArtifactRevision = {
      artifact_id: "artifact-1",
      session_id: "meeting-1",
      transcript_revision_id: "revision-1",
      input_revision: 4,
      /* A template id this build ships, so the document's title resolves in
       * the catalogue rather than through the humanized fallback — this file
       * treats a missing key as a failure. */
      template_id: "meeting-review",
      template_version: 5,
      generation_key: "key-1",
      state: "current",
      generated_at_utc_ms: 1_760_000_200_000,
      content: {
        summary: summary(BOTH_LINES),
        summary_trace: TRACE,
        outline: [],
        decisions: [],
        action_items: [],
        key_questions: [],
        risks: [],
        follow_up_draft: summary("Thanks all."),
      },
    };

    const markup = render(
      <MeetingArtifactPanel
        artifact={artifact}
        doneActionItems={new Set()}
        actionsDisabled={false}
        onJump={noop}
        onActionItemToggle={noop}
      />,
    );

    expect(markup).toContain('aria-label="Jump to the transcript at 0:30"');
  });
});

describe("which segment a summary line dispatches", () => {
  test("each line opens the segment its own entry names", () => {
    const lines = summaryLines(summary(BOTH_LINES), TRACE);

    expect(lines).not.toBeNull();
    expect(lines?.map((line) => line.segmentId)).toEqual([
      "segment-1",
      "segment-2",
    ]);
    expect(lines?.[1]?.startOffsetNs).toBe(30_000_000_000);
  });

  test("a blank line cannot shift a line onto the wrong segment", () => {
    const lines = summaryLines(
      summary(`${FIRST_LINE}\n\n${SECOND_LINE}`),
      /* Line 2 in the raw split, which is what the generator wrote. */
      [TRACE[0], { ...TRACE[1], line: 2 }],
    );

    expect(lines?.map((line) => [line.text, line.segmentId])).toEqual([
      [FIRST_LINE, "segment-1"],
      [SECOND_LINE, "segment-2"],
    ]);
  });

  test("nothing traceable reads as nothing to trace", () => {
    expect(summaryLines(summary(BOTH_LINES), [])).toBeNull();
    expect(summaryLines(summary(BOTH_LINES), null)).toBeNull();
    /* A map that points at no line of this text is as good as no map. */
    expect(
      summaryLines(summary(FIRST_LINE), [{ ...TRACE[1], line: 7 }]),
    ).toBeNull();
  });
});

const SNAPSHOT: MeetingReviewSnapshot = {
  session: {
    session_id: "meeting-1",
    phase: "review_ready",
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
    allowed_actions: ["edit"],
  },
  tracks: [],
  gaps: [],
  speakers: [
    {
      speaker_id: "speaker-1",
      session_id: "meeting-1",
      source_kind: "microphone",
      display_name: "Dana",
      revision: 1,
    },
  ],
  transcript: [
    {
      base: {
        segment_id: "segment-1",
        transcript_revision_id: "revision-1",
        track_id: "track-mic",
        ordinal: 0,
        start_offset_ns: 12_000_000_000,
        end_offset_ns: 14_000_000_000,
        speaker_id: "speaker-1",
        text: "Which tier does the trial convert into?",
        confidence_milli: 940,
      },
      replacement_text: null,
      removed: false,
      edit_revision: null,
      assigned_speaker_id: "speaker-1",
      speaker_assignment: "local_speaker",
    },
    {
      base: {
        segment_id: "segment-2",
        transcript_revision_id: "revision-1",
        track_id: "track-mic",
        ordinal: 1,
        start_offset_ns: 30_000_000_000,
        end_offset_ns: 31_500_000_000,
        speaker_id: "speaker-1",
        text: "I'll send the tier comparison.",
        confidence_milli: 940,
      },
      replacement_text: null,
      removed: false,
      edit_revision: null,
      assigned_speaker_id: "speaker-1",
      speaker_assignment: "local_speaker",
    },
  ],
  notes: [],
  artifacts: [],
  questions: [],
  diarization: {
    status: "succeeded",
    model_id: "diarizer",
    model_version: "1",
    generation_id: "generation-1",
    assigned_segment_count: 2,
  },
  can_export: true,
  remote_cancellation_pending: false,
};

const transcript = (jump: { segmentId: string; nonce: number } | null) =>
  render(
    <TranscriptTab
      snapshot={SNAPSHOT}
      speakerNames={{ "speaker-1": "Dana" }}
      busy={false}
      editable={true}
      jump={jump}
      searchQuery=""
      searchHits={null}
      onSearchQueryChange={noop}
      onSegmentEdit={noop}
      onSpeakerRename={noop}
      onSpeakerMerge={noop}
      onSpeakerCorrect={noop}
    />,
  );

describe("landing on the segment a summary line named", () => {
  test("every segment carries the dom id a jump looks it up by", () => {
    const markup = transcript(null);
    const lines = summaryLines(summary(BOTH_LINES), TRACE);

    expect(lines).not.toBeNull();
    /* The scroll owner resolves `meeting-transcript-segment-<id>`, so a line
     * can only land if the transcript actually publishes that id. */
    for (const line of lines ?? []) {
      expect(markup).toContain(
        `id="meeting-transcript-segment-${line.segmentId}"`,
      );
    }
  });

});

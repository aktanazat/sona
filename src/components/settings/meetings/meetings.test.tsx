import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import type {
  GeneratedMeetingArtifacts,
  MeetingArtifactState,
  MeetingHistorySummary,
  MeetingLedger,
  MeetingLoopRow,
  MeetingPhase,
  MeetingReviewSnapshot,
  MeetingStatusFilter,
  MeetingSuggestion,
  ProcessingFailure,
  ProcessingStatus,
} from "@/bindings";
import { meetingCardStatus } from "./home/meetingCardStatus";
import { MeetingLive } from "./MeetingLive";
import { MeetingStartGate } from "./MeetingStartGate";
import { InsightsTab } from "./review/InsightsTab";
import { MeetingReview, nextReviewTab } from "./MeetingReview";
import { MeetingsHome } from "./MeetingsHome";
import { MeetingLedgerSection } from "./MeetingLedgerSection";
import { currentLedger } from "./meetingLedger";
import { MeetingsSettings } from "./MeetingsSettings";
import { meetingErrorKey, NO_MEETING_FILTER } from "./meetingUtils";
import type { MeetingStartOptions } from "./meetingTypes";

/* First paint of every meetings surface, and the shape of the start flow.
 *
 * Recording is one press. The strings pinned here are the ones that make that
 * true and safe: "Start recording", and the assurance sentence, which has to
 * be on screen next to the button because pressing the button is what the
 * backend records as the operator's acknowledgment. There is no setup screen
 * and no consent checkbox to tick before it; a test that reintroduced either
 * would be describing a flow the product deliberately removed.
 *
 * Static rendering runs no effects, so these are pure prop-to-markup checks:
 * no Tauri command is reachable from here. */

const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
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
    en: { translation: JSON.parse(fs.readFileSync(localeRoot, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

/* Every window root mounts one `TooltipProvider` and the row primitives assume
 * it, so this stands in for the root. Context only: no markup of its own. */
const render = (node: React.ReactElement) =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const occurrences = (markup: string, needle: string) =>
  markup.split(needle).length - 1;

/** Opening tag of the button containing the visible label. */
const buttonTag = (markup: string, label: string) => {
  const labelIndex = markup.indexOf(`>${label}<`);
  if (labelIndex === -1) return "";
  return markup.slice(markup.lastIndexOf("<button", labelIndex), labelIndex);
};

const START_OPTIONS: MeetingStartOptions = {
  title: "Weekly planning",
  origin: "manual",
  suggestionId: null,
  calendarEventKey: null,
  sources: ["microphone", "system_audio"],
  degradedStartPolicy: "abort_if_required_source_fails",
  destination: { kind: "local" },
  preview: null,
};

const SUGGESTION: MeetingSuggestion = {
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
};

const SUMMARY: MeetingHistorySummary = {
  kind: "meeting",
  session_id: "meeting-1",
  title: "Weekly planning",
  phase: "review_ready",
  created_at_utc_ms: 1_760_000_000_000,
  capture_completeness: "complete",
  processing_status: { kind: "succeeded" },
};

const cited = (text: string, segmentId: string | null) => ({
  text,
  citations:
    segmentId === null
      ? []
      : [
          {
            segment_id: segmentId,
            start_offset_ns: 12_000_000_000,
            end_offset_ns: 14_000_000_000,
          },
        ],
});

const SNAPSHOT: MeetingReviewSnapshot = {
  session: {
    session_id: "meeting-1",
    phase: "review_ready",
    revision: 4,
    title: "Weekly planning",
    started_at_utc_ms: 1_760_000_000_000,
    elapsed_offset_ns: 1_845_000_000_000,
    sources: [
      {
        track_id: "track-mic",
        source_kind: "microphone",
        required: true,
        availability: "available",
        health: "stopped",
        format: { sample_rate_hz: 16_000, channels: 1 },
        last_durable_offset_ns: 1_845_000_000_000,
        gap_count: 0,
      },
      {
        track_id: "track-system",
        source_kind: "system_audio",
        required: true,
        availability: "permission_required",
        health: "failed",
        format: null,
        last_durable_offset_ns: null,
        gap_count: 2,
      },
    ],
    open_capture_window_started_at_ns: null,
    capture_completeness: "partial",
    storage: "available",
    processing_status: { kind: "succeeded" },
    retention_deadline_utc_ms: null,
    allowed_actions: ["edit", "regenerate", "ask_question", "export", "delete"],
  },
  tracks: [],
  gaps: [
    {
      track_id: "track-system",
      epoch: 1,
      start_offset_ns: 60_000_000_000,
      end_offset_ns: 90_000_000_000,
      reason: "permission_lost",
      dropped_frames: 128,
    },
  ],
  speakers: [
    {
      speaker_id: "speaker-1",
      session_id: "meeting-1",
      source_kind: "microphone",
      display_name: "Aktan",
      revision: 1,
    },
    {
      speaker_id: "speaker-2",
      session_id: "meeting-1",
      source_kind: "system_audio",
      display_name: "Guest",
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
        text: "We ship the meetings redesign this week.",
        confidence_milli: 940,
      },
      replacement_text: null,
      removed: false,
      edit_revision: null,
      assigned_speaker_id: "speaker-1",
      speaker_assignment: "local_speaker",
    },
  ],
  notes: [
    {
      note_id: "note-1",
      session_id: "meeting-1",
      start_offset_ns: 30_000_000_000,
      end_offset_ns: null,
      body: "Ask about the export format.",
      revision: 1,
      created_at_utc_ms: 1_760_000_100_000,
      updated_at_utc_ms: 1_760_000_100_000,
    },
  ],
  artifacts: [
    {
      artifact_id: "artifact-1",
      session_id: "meeting-1",
      transcript_revision_id: "revision-1",
      input_revision: 4,
      template_id: "standard",
      template_version: 1,
      generation_key: "key-1",
      state: "current",
      generated_at_utc_ms: 1_760_000_200_000,
      content: {
        summary: cited("The team agreed to ship this week.", "segment-1"),
        outline: [
          {
            title: cited("Release plan", "segment-1"),
            detail: cited("Cut on Thursday.", null),
          },
        ],
        decisions: [cited("Ship on Thursday.", "segment-1")],
        action_items: [
          {
            text: cited("Write the release notes.", "segment-1"),
            owner_text: "Aktan",
            due_text: null,
          },
        ],
        key_questions: [cited("Who signs off?", null)],
        risks: [cited("System audio permission is missing.", "segment-1")],
        follow_up_draft: cited("Thanks all, notes below.", null),
      },
    },
  ],
  questions: [
    {
      question_id: "question-1",
      session_id: "meeting-1",
      scope: { kind: "this_meeting" },
      question: "What did we decide about the release?",
      state: "supported",
      answer: "You agreed to ship on Thursday.",
      citations: [
        {
          kind: "transcript",
          session_id: "meeting-1",
          entity_id: "segment-1",
          start_offset_ns: 12_000_000_000,
          end_offset_ns: 14_000_000_000,
        },
      ],
      input_revision: 4,
      revision: 1,
      created_at_utc_ms: 1_760_000_300_000,
      through_offset_ns: null,
      provisional: false,
    },
  ],
  diarization: {
    status: "succeeded",
    model_id: "diarizer",
    model_version: "1",
    generation_id: "generation-1",
    assigned_segment_count: 1,
  },
  can_export: true,
  remote_cancellation_pending: false,
};

const LIVE_SNAPSHOT: MeetingReviewSnapshot = {
  ...SNAPSHOT,
  session: {
    ...SNAPSHOT.session,
    phase: "capturing_recording",
    allowed_actions: ["pause", "stop", "discard"],
  },
  artifacts: [],
  questions: [],
};

const noop = () => {};

/* The promise that has to be on screen wherever the consent flags can be
 * sent, in the form the renderer emits it. */
const ASSURANCE =
  "Records your Mac&#x27;s audio locally. Nothing joins the call.";

const homeMarkup = (
  overrides: Partial<React.ComponentProps<typeof MeetingsHome>>,
) =>
  render(
    <MeetingsHome
      suggestions={[]}
      recovery={[]}
      meetings={[]}
      loading={false}
      paging={false}
      hasMore={false}
      page={1}
      filter={NO_MEETING_FILTER}
      error={null}
      sources={["microphone", "system_audio"]}
      starting={false}
      focusStart={false}
      onStart={noop}
      onImport={noop}
      onStartSuggestion={noop}
      onStartEvent={noop}
      onOpenMeeting={noop}
      onFinalizeRecovery={noop}
      onDiscardRecovery={noop}
      onFilterChange={noop}
      onNextPage={noop}
      onPreviousPage={noop}
      onExportMeeting={noop}
      onExportLedger={noop}
      onDeleteMeeting={noop}
      onRetry={noop}
      onOpenSettings={noop}
      {...overrides}
    />,
  );

describe("meeting command errors", () => {
  test("engine failures show the processing failure message", () => {
    expect(i18n.t(meetingErrorKey("engine_failure"))).toBe("Processing failed");
  });
});

describe("meetings section", () => {
  test("mounts on first paint and waits on the list skeleton", () => {
    const markup = render(<MeetingsSettings onOpenSettings={noop} />);
    expect(markup).toContain('aria-label="Loading meeting history…"');
    expect(markup).toContain(">Meetings</h1>");
    expect(markup).toContain(">Record</button>");
  });
});

describe("starting a meeting", () => {
  /* One press, on the title line, and one sentence under it. The card this
   * replaced carried three things that were not the press: two source chips
   * answering a question that has one answer, a retention sentence only
   * Settings can change, and an Import competing for the same corner. */
  test("the page is Record, and the sentence that says what Record does", () => {
    const markup = homeMarkup({});

    expect(occurrences(markup, ">Record</button>")).toBe(1);
    expect(occurrences(markup, ASSURANCE)).toBe(1);
    expect(markup).not.toContain("Microphone");
    expect(markup).not.toContain("System audio");
    expect(markup).not.toContain("Kept 30 days");
    // The shell's shortcut and the palette both move focus to this press.
    expect(markup).toContain('id="meeting-start-button"');
  });

  test("a press in flight cannot be pressed twice", () => {
    const markup = homeMarkup({ starting: true });

    expect(buttonTag(markup, "Starting…")).toContain('disabled=""');
    expect(occurrences(markup, ">Record</button>")).toBe(0);
  });

  /* Import, the recordings folder and the bin are one menu beside Record. A
   * static render never mounts a menu's portal, so what this proves is that
   * all three left the page's surface for the one trigger that owns them. */
  test("the verbs nobody performs weekly are behind the page's one menu", () => {
    const markup = homeMarkup({});

    expect(occurrences(markup, 'aria-label="More"')).toBe(1);
    expect(markup).not.toContain(">Import</button>");
    expect(markup).not.toContain("Open recordings folder");
    expect(markup).not.toContain("Recently deleted");
  });

  // The wizard is gone: no setup screen, and no acknowledgement to tick
  // before the press that is itself the acknowledgement.
  test("nothing stands between the press and the recording", () => {
    const markup = homeMarkup({});

    expect(occurrences(markup, "Check recording setup")).toBe(0);
    expect(
      occurrences(markup, "I have permission to capture this meeting."),
    ).toBe(0);
  });
});

describe("meetings list", () => {
  test("shows a loading skeleton before the first page lands", () => {
    const markup = homeMarkup({ loading: true });
    expect(markup).toContain('aria-label="Loading meeting history…"');
    expect(markup).toContain('data-slot="skeleton"');
    expect(occurrences(markup, "No meetings yet")).toBe(0);
  });

  test("an empty history is absence, not a second call to action", () => {
    const markup = homeMarkup({});
    expect(markup).toContain("No meetings yet");
    /* Absence is the whole message. The state used to add "Start local notes
     * when you are ready to capture a meeting." under a heading that already
     * says it, three rows below a Start button that already offers it. */
    expect(markup).not.toContain("Start local notes when you are ready");
    expect(occurrences(markup, ">Start recording</button>")).toBe(1);
  });

  test("a detected meeting starts from its own row, without repeating the promise", () => {
    const markup = homeMarkup({ suggestions: [SUGGESTION] });
    expect(markup).toContain("A meeting may be active in Zoom.");
    expect(occurrences(markup, ">Start recording</button>")).toBe(2);
    /* The assurance sentence lives beside the page's own Start and nowhere
     * else: one sentence never appears twice on one screen. */
    expect(occurrences(markup, ASSURANCE)).toBe(1);
    /* And the evidence is shown as the measurement it is — an APP fact naming
     * the app — rather than as "Sona noticed a meeting app in use.", which was
     * the section heading written out a second time as prose. */
    expect(markup).not.toContain("Sona noticed");
    expect(markup).toContain(">App<");
    expect(markup).toContain(">Zoom<");
  });

  /* The row render matrix. A row in the day log says three things — what the
   * meeting was called, how long it ran, and the time of day it started — and
   * `data-headline` is the row stating its own provenance, which keeps these
   * assertions about behaviour rather than about prose. */
  const row = (overrides: Partial<MeetingHistorySummary>) =>
    homeMarkup({ meetings: [{ ...SUMMARY, ...overrides }] });

  test("a finished row is its title, its length and its time, and no chip", () => {
    const markup = row({
      headline: { kind: "ledger", text: "Pricing is open again." },
      speaker_labels: ["Ada", "Grace"],
      sources: ["microphone", "system_audio"],
      recorded_duration_ms: 192_000,
    });
    expect(markup).toContain("Weekly planning");
    expect(markup).toContain('data-headline="ledger"');
    expect(markup).toContain("3m 12s");
    // The heading over the group carries the date, so the row carries a clock.
    expect(occurrences(markup, 'data-slot="meeting-day"')).toBe(1);
    /* Nothing a finished meeting does not need: no status chip, no speaker
     * bubbles, no source glyphs and no second line. The summary it used to
     * print is the row's hover title, and the meeting is one click away. */
    expect(occurrences(markup, 'data-slot="meeting-status"')).toBe(0);
    expect(occurrences(markup, 'data-slot="meeting-person"')).toBe(0);
    expect(markup).not.toContain(">Ada, Grace<");
    expect(markup).not.toContain("Pricing is open again.</span>");
    expect(markup).toContain(
      'title="Weekly planning — Pricing is open again."',
    );
  });

  test("only an unfinished meeting wears a chip, and it names the state", () => {
    const chip = (overrides: Partial<MeetingHistorySummary>) => {
      const markup = row(overrides);
      const at = markup.indexOf('data-slot="meeting-status"');
      return at === -1 ? "" : markup.slice(at, markup.indexOf("</span>", at));
    };
    expect(
      chip({
        phase: "capturing_recording",
        processing_status: { kind: "pending" },
      }),
    ).toContain('data-status="live"');
    expect(
      chip({
        phase: "starting",
        processing_status: { kind: "pending" },
      }),
    ).toContain('data-status="live"');
    expect(
      chip({ phase: "processing", processing_status: { kind: "running" } }),
    ).toContain('data-status="processing"');
    expect(
      chip({
        processing_status: { kind: "failed", reason: "engine_failure" },
      }),
    ).toContain('data-status="needs_attention"');
    // Recovery needs action even before processing reports a failure.
    expect(
      chip({
        phase: "recovery_required",
        processing_status: { kind: "pending" },
      }),
    ).toContain('data-status="needs_attention"');
    // A meeting that is ready to read says nothing about being ready.
    expect(chip({})).toBe("");
  });

  test("the log is grouped by the day each meeting was recorded", () => {
    const markup = homeMarkup({
      meetings: [
        SUMMARY,
        { ...SUMMARY, session_id: "meeting-2", title: "Standup" },
        {
          ...SUMMARY,
          session_id: "meeting-3",
          title: "Retro",
          created_at_utc_ms: SUMMARY.created_at_utc_ms - 172_800_000,
        },
      ],
    });
    // Two days, three rows: same-day meetings share one heading.
    expect(occurrences(markup, 'data-slot="meeting-day"')).toBe(2);
    expect(occurrences(markup, 'data-slot="meeting-entry"')).toBe(3);
    expect(markup.indexOf("Standup")).toBeLessThan(markup.indexOf("Retro"));
  });

  test("the row keeps its provenance without printing a word count", () => {
    expect(
      row({ headline: { kind: "summary", text: "We picked Postgres." } }),
    ).toContain('data-headline="summary"');
    const words = row({ headline: { kind: "words", words: 1_284 } });
    expect(words).toContain('data-headline="words"');
    expect(occurrences(words, "words transcribed")).toBe(0);
    expect(row({ headline: { kind: "none" } })).toContain(
      'data-headline="none"',
    );
  });

  test("a capture with nothing to report reports nothing, never a zero", () => {
    const markup = row({
      recorded_duration_ms: null,
      sources: [],
      speaker_labels: [],
      capture_completeness: "partial",
    });
    expect(occurrences(markup, "0s")).toBe(0);
    expect(occurrences(markup, "MIC")).toBe(0);
    /* Completeness is a fact about the recording, read on the meeting itself.
     * A log row is not where a caveat about audio belongs. */
    expect(occurrences(markup, "Partial")).toBe(0);
  });

  /* One menu per row, and no column of chrome down the list: the trigger
   * appears on hover, on keyboard focus anywhere in the row, and while its
   * own menu is open - and it stays in the tab order the whole time, for the
   * reader who never touches a mouse. */
  test("each row keeps its actions behind one menu that is not always shown", () => {
    const ledger = row({
      headline: { kind: "ledger", text: "Pricing is open again." },
    });

    expect(occurrences(ledger, 'aria-label="Meeting actions"')).toBe(1);
    expect(ledger).toContain("opacity-0");
    expect(ledger).toContain("group-focus-within:opacity-100");
    expect(ledger).not.toContain("Export ledger page");
  });

  test("the list keeps one control: the title the reader typed", () => {
    const markup = homeMarkup({
      meetings: [SUMMARY],
      filter: { ...NO_MEETING_FILTER, title_query: "sync" },
    });

    expect(markup).toContain('aria-label="Search meetings"');
    expect(markup).toContain('value="sync"');
    /* The status picker, the window picker and Clear are gone: a status the
     * reader cannot change and a window they can read off the day headings
     * were three controls asking about one list. Retention went with them, to
     * the one page that can change it. */
    expect(markup).not.toContain("Clear filters");
    expect(markup).not.toContain(">Status<");
    expect(markup).not.toContain(">Time<");
    expect(markup).not.toContain("Kept 30 days");
  });

  test("a search that matches nothing says so with what was typed", () => {
    const empty = homeMarkup({
      filter: { ...NO_MEETING_FILTER, title_query: "sync" },
    });

    expect(empty).toContain("No meeting title matches “sync”.");
    expect(empty).not.toContain("Meetings you record appear here.");
  });

  /* An interrupted launch used to leave its meetings in a section of their
   * own above the log, which printed the same rows the log prints - the same
   * meeting, twice on one screen. They are the first group of the one list
   * now, which also settles what paging used to decide: a meeting stranded
   * behind fifty newer ones was on page three, where nobody looked. */
  test("an unfinished meeting is pinned to the top of the one list, once", () => {
    const markup = homeMarkup({
      recovery: [STRANDED],
      meetings: [STRANDED, SUMMARY],
    });

    expect(markup).toContain("Unfinished meetings");
    expect(occurrences(markup, 'data-slot="meeting-entry"')).toBe(2);
    expect(markup.indexOf("Yesterday&#x27;s standup")).toBeLessThan(
      markup.indexOf("Weekly planning"),
    );
  });

  /* A typed query is the reader's ordering, not the page's: pinning rows
   * nobody searched for above the store's answer would be the page arguing
   * with its own search box. */
  test("a search drops the pin, so the answer is only what matched", () => {
    const markup = homeMarkup({
      recovery: [STRANDED],
      meetings: [SUMMARY],
      filter: { ...NO_MEETING_FILTER, title_query: "planning" },
    });

    expect(markup).not.toContain("Unfinished meetings");
    expect(occurrences(markup, 'data-slot="meeting-entry"')).toBe(1);
  });

  /* `disabled=""` and not "disabled": the Button primitive carries
   * `disabled:`-prefixed utility classes, so the loose substring matches every
   * button ever rendered and proves nothing. */
  test("the pager states the page it is on and nothing it cannot know", () => {
    const first = homeMarkup({ meetings: [SUMMARY], hasMore: true });
    expect(first).toContain("Page 1");
    expect(buttonTag(first, "Newer")).toContain('disabled=""');
    expect(buttonTag(first, "Older")).not.toContain('disabled=""');

    const third = homeMarkup({ meetings: [SUMMARY], hasMore: false, page: 3 });
    expect(third).toContain("Page 3");
    expect(buttonTag(third, "Newer")).not.toContain('disabled=""');
    expect(buttonTag(third, "Older")).toContain('disabled=""');
    // No total exists behind a cursor, so no total is claimed.
    expect(occurrences(third, "of 3")).toBe(0);

    // One page and nothing after it needs no pager at all.
    const only = homeMarkup({ meetings: [SUMMARY] });
    expect(occurrences(only, "Page 1")).toBe(0);
  });

  test("a page in flight disables both moves rather than queueing them", () => {
    const markup = homeMarkup({
      meetings: [SUMMARY],
      hasMore: true,
      page: 2,
      paging: true,
    });
    expect(buttonTag(markup, "Newer")).toContain('disabled=""');
    expect(buttonTag(markup, "Older")).toContain('disabled=""');
  });

  /* The filters are the store's, not the view's. A row the current filter text
   * would exclude still renders, because the page on screen is exactly what
   * `meeting_list` answered with — this is the assertion that fails the moment
   * anyone reintroduces client-side filtering over an already-fetched page.
   * That the store honours each filter value is proved in Rust:
   * meeting::store::tests::listed_status_filter_reads_stored_phase_and_processing_status,
   * listed_time_window_counts_local_calendar_days_including_today, and
   * listed_title_query_matches_a_substring_and_treats_wildcards_literally. */
  test("the page on screen is the store's answer, not a view over it", () => {
    const markup = homeMarkup({
      meetings: [SUMMARY],
      filter: {
        status: "failed",
        window: "today",
        title_query: "nothing in this title",
      },
    });
    expect(markup).toContain("Weekly planning");
    expect(occurrences(markup, "No meetings match")).toBe(0);
  });
});

describe("the start gate", () => {
  const gateMarkup = (
    overrides: Partial<React.ComponentProps<typeof MeetingStartGate>> = {},
  ) =>
    render(
      <MeetingStartGate
        snapshot={{
          ...SNAPSHOT,
          session: { ...SNAPSHOT.session, phase: "preflight" },
        }}
        options={START_OPTIONS}
        refreshing={false}
        starting={false}
        onRefresh={noop}
        onCancel={noop}
        onStart={noop}
        {...overrides}
      />,
    );

  test("an unavailable required source names itself and blocks the press", () => {
    const markup = gateMarkup();
    expect(markup).toContain(">Recording did not start</h1>");
    // The fixture's system audio needs permission, so it is the blocker.
    expect(markup).toContain("System audio");
    expect(markup).toContain("Permission required");
    // Recording anyway is a partial record, and says so before the press.
    expect(markup).toContain(
      "The meeting is marked partial, and the missing source is named in it.",
    );
    expect(buttonTag(markup, "Record without it")).toContain("disabled");
  });

  test("the assurance is on screen wherever the consent flags can be sent", () => {
    expect(gateMarkup()).toContain(ASSURANCE);
  });

  test("a session with nothing blocking offers the one press directly", () => {
    const markup = gateMarkup({
      snapshot: {
        ...SNAPSHOT,
        session: {
          ...SNAPSHOT.session,
          phase: "preflight",
          processing_status: { kind: "pending" },
          preflight_local_processing: "available",
          allowed_actions: ["refresh_preflight", "cancel_preflight", "start"],
          sources: SNAPSHOT.session.sources.map((source) => ({
            ...source,
            availability: "available",
          })),
        },
      },
    });
    expect(markup).toContain(">Ready to record</h1>");
    expect(
      occurrences(buttonTag(markup, "Start recording"), 'disabled=""'),
    ).toBe(0);
    expect(occurrences(markup, "Record without it")).toBe(0);
    expect(markup).toContain(">Available<");
    expect(markup).not.toContain("Waiting for processing");
  });

  test("a stale preflight cannot render a press that its snapshot rejects", () => {
    const markup = gateMarkup({
      snapshot: {
        ...SNAPSHOT,
        session: {
          ...SNAPSHOT.session,
          phase: "preflight",
          allowed_actions: ["refresh_preflight", "cancel_preflight"],
          sources: SNAPSHOT.session.sources.map((source) => ({
            ...source,
            availability: "available",
          })),
        },
      },
    });
    expect(buttonTag(markup, "Start recording")).toContain("disabled");
    expect(markup).toContain(
      "This action is not available in the current phase.",
    );
  });
});

describe("live capture", () => {
  test("names the capture state in words and offers pause, stop and discard", () => {
    const markup = render(
      <MeetingLive
        snapshot={LIVE_SNAPSHOT}
        pendingAction={null}
        onPause={noop}
        onResume={noop}
        onStop={noop}
        onDiscard={noop}
        onCreateNote={noop}
      />,
    );
    /* One state word. The surface used to carry a badge reading "Active
     * capture" beside the phase word, which is the same state said twice. */
    expect(markup).toContain("Recording");
    expect(markup).toContain(">Pause<");
    expect(markup).toContain(">Stop<");
    expect(markup).toContain(">Discard</button>");
    expect(markup).toContain("Add timestamped note");
    // Reduced motion has nothing to switch off: the mark never animates.
    expect(occurrences(markup, "animate-")).toBe(0);
  });
});

/* The generated panel on its own. Module-scoped because two describes read
 * it: the panel's, and the review's, which has to show that a ledger never
 * takes the summary or the actions away from it. */
const insightsMarkup = (
  overrides: Partial<React.ComponentProps<typeof InsightsTab>>,
) =>
  render(
    <InsightsTab
      snapshot={SNAPSHOT}
      busy={false}
      editable
      canRegenerate
      newNote=""
      analytics={null}
      speakerNames={{}}
      doneActionItems={new Set()}
      onNewNoteChange={noop}
      onCreateNote={noop}
      onNoteUpdate={noop}
      onNoteDelete={noop}
      onRegenerate={noop}
      onJumpToSegment={noop}
      onActionItemToggle={noop}
      onRefresh={async () => {}}
      onAnalyticsRefresh={async () => {}}
      onOpenSettings={noop}
      {...overrides}
    />,
  );

describe("meeting review", () => {
  const reviewMarkup = (
    overrides: Partial<React.ComponentProps<typeof MeetingReview>> = {},
  ) =>
    render(
      <MeetingReview
        snapshot={SNAPSHOT}
        lastReceipt={null}
        pendingAction={null}
        onBack={noop}
        onTitleSet={noop}
        onSpeakerRename={noop}
        onSpeakerMerge={noop}
        onSegmentEdit={noop}
        onNoteCreate={noop}
        onNoteUpdate={noop}
        onNoteDelete={noop}
        onRegenerate={noop}
        onExport={noop}
        onRemoteCancel={noop}
        onDelete={noop}
        onRefresh={async () => {}}
        onOpenSettings={noop}
        {...overrides}
      />,
    );
  const markup = reviewMarkup();
  /* Radix mounts only the open panel's children, so the transcript's own
   * contract is read on a meeting with nothing written about it at all —
   * neither generated notes nor typed ones — which is exactly the meeting
   * that opens on it. */
  const onTranscript = reviewMarkup({
    snapshot: { ...SNAPSHOT, artifacts: [], notes: [] },
  });

  test("opens on what was written from the meeting, all four panels reachable", () => {
    expect(markup).toContain('aria-label="Meeting review sections"');
    /* The strip is Radix now, so the ids that wire a tab to its panel are
     * generated rather than authored — asserting them would pin a detail the
     * kit owns. The contract is the roles and the selected state. */
    expect(occurrences(markup, 'role="tab"')).toBe(3);
    /* Every tab is a word you could say out loud. Asking the meeting a
     * question moved to the chat column, so there is no Questions tab. */
    for (const label of ["Transcript", "Insights", "Ledger"]) {
      expect(markup).toContain(`>${label}<`);
    }
    expect(markup).not.toContain(">Questions<");
    /* All three panels are rendered so the strip is navigable without JS, but
     * only the chosen one is active. Generated notes are what somebody came
     * back for: the transcript is the evidence behind them, one press away. */
    expect(occurrences(markup, 'role="tabpanel"')).toBe(3);
    expect(occurrences(markup, 'data-state="active"')).toBe(2);
    expect(buttonTag(markup, "Insights")).toContain('data-state="active"');
    expect(buttonTag(markup, "Transcript")).not.toContain(
      'data-state="active"',
    );
    /* The open panel is the generated one, so its words are the ones on the
     * page: the transcript's are behind its trigger. */
    expect(markup).toContain("The team agreed to ship this week.");
    expect(markup).not.toContain("We ship the meetings redesign this week.");
  });

  test("a meeting with nothing written about it yet opens on its transcript", () => {
    expect(buttonTag(onTranscript, "Transcript")).toContain(
      'data-state="active"',
    );
    expect(buttonTag(onTranscript, "Insights")).not.toContain(
      'data-state="active"',
    );
    expect(onTranscript).toContain("We ship the meetings redesign this week.");
  });

  /* A meeting you typed notes into during the call has something to read on
   * Insights even before D19 has written a word, so that is where it opens.
   * The shipped build read `artifacts` alone and left those readers on the
   * transcript, looking at the evidence for notes they could not see. */
  test("notes typed during the meeting open it on Insights too", () => {
    const onNotes = reviewMarkup({ snapshot: { ...SNAPSHOT, artifacts: [] } });
    expect(buttonTag(onNotes, "Insights")).toContain('data-state="active"');
    expect(buttonTag(onNotes, "Transcript")).not.toContain(
      'data-state="active"',
    );
  });

  /* The race this rule exists for, as the sequence the screen actually sees:
   * processing finishes while the review is open, so the record arrives empty
   * and fills in. A `useState` initialiser reads only the first of these.
   * What fills in decides where it opens: the ledger is the default reading
   * of a finished meeting, and Insights is the fallback for a record that has
   * words but no ledger. `LEDGER` is defined further down the file; tests run
   * after the module has finished evaluating, so it is in reach here. */
  test("a record whose notes arrive after the screen does still opens on them", () => {
    const arriving = { ...SNAPSHOT, artifacts: [], notes: [] };

    // First snapshot: nothing to read, nobody has decided, transcript renders.
    expect(nextReviewTab(null, arriving)).toBeNull();
    // Second snapshot, same mounted screen: notes landed without a ledger.
    expect(nextReviewTab(null, SNAPSHOT)).toBe("insights");
    // Or with one, which is where a finished meeting is read from.
    expect(nextReviewTab(null, ledgerSnapshot(LEDGER))).toBe("ledger");
    // A ledger on a revision that is no longer current is no ledger.
    const [artifact] = ledgerSnapshot(LEDGER).artifacts;
    expect(
      nextReviewTab(null, {
        ...SNAPSHOT,
        artifacts: [{ ...artifact, state: "out_of_date" }],
      }),
    ).toBe("insights");
  });

  test("a decision, once made, survives every later snapshot", () => {
    // The reader picked the transcript; a finished processing pass may not
    // move them off it.
    expect(nextReviewTab("transcript", SNAPSHOT)).toBe("transcript");
    // And deleting the last note may not pull them back off Insights.
    expect(
      nextReviewTab("insights", { ...SNAPSHOT, artifacts: [], notes: [] }),
    ).toBe("insights");
    // Nor may a ledger arriving pull a reader off the tab they are on.
    expect(nextReviewTab("insights", ledgerSnapshot(LEDGER))).toBe("insights");
    expect(nextReviewTab("transcript", ledgerSnapshot(LEDGER))).toBe(
      "transcript",
    );
  });

  /* Upstream's "should not fire" evals, in Sona's terms. Sona reads a ledger
   * from every meeting rather than on a phrase, so the only trigger rule left
   * is the negative one: a ledger is more than a summary or an action list
   * asked for, and it never stands in for either. The ledger keeps its own
   * tab; Insights keeps the summary and the actions. */
  test("a ledger never replaces the summary or the action list", () => {
    const withLedger = ledgerSnapshot(LEDGER);
    const review = reviewMarkup({ snapshot: withLedger });
    expect(buttonTag(review, "Ledger")).toContain('data-state="active"');
    expect(buttonTag(review, "Insights")).not.toContain('data-state="active"');
    /* The open panel is the ledger's, and the summary is not in it. */
    expect(review).toContain(LEDGER.headline);
    expect(review).not.toContain("The team agreed to ship this week.");
    /* Insights, on the same record, still carries the summary and the
     * actions, and none of the ledger. The summary is the document's lede
     * now, so it has no label of its own to look for — the sentence is the
     * assertion. */
    const insights = insightsMarkup({ snapshot: withLedger });
    expect(insights).toContain("The team agreed to ship this week.");
    expect(insights).toContain(">Action items<");
    expect(insights).toContain("Write the release notes.");
    expect(insights).not.toContain(LEDGER.headline);
    expect(insights).not.toContain("Pricing tiers");
  });

  test("the title is the page's heading, and the only way to a field", () => {
    expect(markup).toContain(">Weekly planning</button>");
    expect(markup).toContain('title="Rename this meeting"');
    /* No microlabel over it, no field, and no Save: D19 writes the title, so
     * correcting it is the exception rather than the first thing offered. */
    expect(markup).not.toContain(">Meeting title<");
    expect(markup).not.toContain('id="meeting-review-title"');
  });

  test("the header states the recording once, and measures it in a sentence", () => {
    expect(markup).toContain(">Ready for review<");
    /* Sentence case, and the word that changes what the record can be trusted
     * for. "Partial" on its own was a machine's shorthand for it. */
    expect(markup).toContain(">Partial recording<");
    /* When it started and how long it ran are one quiet line, not a labelled
     * ELAPSED measurement beside the state. */
    expect(markup).toContain("Started ");
    expect(markup).toContain(" · 30:45");
    expect(markup).not.toContain(">Elapsed<");
  });

  test("the transcript panel reads as prose, and still carries its coverage", () => {
    expect(onTranscript).toContain('id="meeting-transcript-segment-segment-1"');
    /* The words are words: no field per turn, and no destructive control on a
     * surface somebody is reading. */
    expect(onTranscript).not.toContain("<textarea");
    expect(onTranscript).not.toContain(">Remove this turn<");
    expect(onTranscript).toContain('title="Edit this turn"');
    expect(onTranscript).toContain("Permission lost");
    expect(onTranscript).toContain("128 frames dropped");
    expect(onTranscript).toContain("Some of this audio is missing.");
  });

  test("speakers are a row of names, and no roster machinery", () => {
    expect(occurrences(onTranscript, 'data-slot="speaker-chip"')).toBe(2);
    expect(onTranscript).toContain(">Aktan</button>");
    expect(onTranscript).toContain(">Guest</button>");
    expect(onTranscript).toContain('title="Rename this speaker"');
    /* No labelled field, no Save, and no pair of merge dropdowns. */
    expect(onTranscript).not.toContain(">Speaker name<");
    expect(onTranscript).not.toContain(">Merge speakers<");
    /* Separation worked here, so the roster says nothing about it: the names
     * are the result. "Diarization" reaches no reader either way. */
    expect(onTranscript).not.toContain("Speakers are up to date");
    expect(onTranscript).not.toContain("iarization");
  });

  test("a meeting whose speakers were never separated says so, once and quietly", () => {
    const undiarized = reviewMarkup({
      snapshot: {
        ...SNAPSHOT,
        artifacts: [],
        // Nothing written about it, so the transcript is the open panel and
        // the roster note is reachable in this render.
        notes: [],
        diarization: {
          ...SNAPSHOT.diarization,
          status: "not_requested",
          assigned_segment_count: 0,
        },
      },
    });
    expect(occurrences(undiarized, "Speakers not separated")).toBe(1);
  });

  test("search is one live field on the transcript, with no card and no submit", () => {
    expect(onTranscript).toContain('placeholder="Search this meeting"');
    expect(onTranscript).toContain('aria-label="Search this meeting"');
    expect(onTranscript).not.toContain(">Exact search<");
    expect(onTranscript).not.toContain(">Search</button>");
  });

  test("keeps the export actions on the record, and no delete beside them", () => {
    expect(markup).toContain(">Export Markdown<");
    expect(markup).toContain(">Export JSON<");
    expect(markup).toContain(">More<");
    /* The load-bearing half: a red delete button coming back to a surface
     * somebody is reading fails here. Deleting a meeting lives behind the
     * menu, which mounts nothing until it is opened. */
    expect(markup).not.toContain(">Delete meeting<");
  });
});

describe("insights panel", () => {
  /* One artifact, rendered through the tab that carries it, with the parts a
   * case is about spelled out. The base is the honest hard case: a meeting
   * that produced a summary and nothing else. */
  const artifactWith = (
    content: Partial<GeneratedMeetingArtifacts>,
    state: MeetingArtifactState = "current",
    templateId = "standard",
  ) =>
    insightsMarkup({
      snapshot: {
        ...SNAPSHOT,
        artifacts: [
          {
            ...SNAPSHOT.artifacts[0],
            template_id: templateId,
            state,
            content: {
              summary: cited("The team agreed to ship this week.", "segment-1"),
              outline: [],
              decisions: [],
              action_items: [],
              key_questions: [],
              risks: [],
              follow_up_draft: cited("Thanks all, notes below.", null),
              ...content,
            },
          },
        ],
      },
    });

  test("renders every generated section with citations as jump controls", () => {
    const markup = insightsMarkup({});
    /* The summary is the document's lede and carries no label; every other
     * section is named above its rows. */
    for (const heading of [
      "Topics",
      "Decisions",
      "Action items",
      "Key questions",
      "Risks",
      "Follow-up",
    ]) {
      expect(markup).toContain(`>${heading}<`);
    }
    expect(markup).toContain("The team agreed to ship this week.");
    expect(markup).toContain(">Owner: Aktan<");
    expect(markup).toContain(">0:12</button>");
    expect(markup).toContain(">Regenerate<");
    // Manual notes stay separate from what was generated.
    expect(markup).toContain("Ask about the export format.");
    expect(markup).toContain(">0:30<");
  });

  test("the notes are titled by their shape, and a current pass says nothing", () => {
    const markup = artifactWith({});
    /* The screenshotted defect: "Template: meeting-review" as the heading of
     * the document, and "Current" beside it on every artifact that is not
     * broken — a word that is always there tells a reader nothing. */
    expect(markup).toContain(">Standard<");
    expect(markup).not.toContain("Template: ");
    expect(markup).not.toContain(">Current<");
    /* A template this build knows is named with the same word the picker
     * offered when somebody chose it. */
    expect(artifactWith({}, "current", "meeting-one-on-one")).toContain(
      ">One-to-one<",
    );
  });

  test("notes that no longer match the transcript say so under their title", () => {
    expect(artifactWith({}, "out_of_date")).toContain(">Out of date<");
    expect(artifactWith({}, "failed")).toContain(">Generation failed<");
  });

  test("a section with no rows is off the page, and four empty ones say it once", () => {
    const empty = artifactWith({});
    for (const heading of [
      "Topics",
      "Decisions",
      "Action items",
      "Key questions",
      "Risks",
    ]) {
      expect(empty).not.toContain(`>${heading}<`);
    }
    /* "Decisions / None" four times over is what this replaced. */
    expect(empty).not.toContain(">None<");
    expect(
      occurrences(
        empty,
        "No decisions, action items, questions, or risks were recorded.",
      ),
    ).toBe(1);
    /* One decision brings its own section back and makes that line wrong. */
    const oneDecision = artifactWith({
      decisions: [cited("Ship on Thursday.", "segment-1")],
    });
    expect(oneDecision).toContain(">Decisions<");
    expect(oneDecision).not.toContain(
      "No decisions, action items, questions, or risks were recorded.",
    );
  });

  test("an action item states the owner and the date somebody recorded, and no more", () => {
    const action = (owner: string | null, due: string | null) => ({
      action_items: [
        {
          text: cited("Write the release notes.", "segment-1"),
          owner_text: owner,
          due_text: due,
        },
      ],
    });

    expect(artifactWith(action("Dana", "Thursday"))).toContain(
      ">Owner: Dana · Due: Thursday<",
    );
    /* The absences used to be printed as facts: an item nobody owns read
     * "Owner: Unassigned", and one with no date read "Due: No due date". */
    expect(artifactWith(action("Dana", null))).toContain(">Owner: Dana<");
    expect(artifactWith(action("Dana", null))).not.toContain("Due:");
    expect(artifactWith(action(null, "Thursday"))).toContain(">Due: Thursday<");
    expect(artifactWith(action(null, "Thursday"))).not.toContain("Owner:");
    expect(artifactWith(action(null, null))).not.toContain("Owner:");
    expect(artifactWith(action(null, null))).not.toContain("Due:");
  });

  test("manual notes read as notes until somebody presses one", () => {
    const markup = insightsMarkup({});
    expect(markup).toContain("Ask about the export format.");
    expect(markup).toContain(">0:30<");
    /* No field and no destructive control on a surface somebody is reading:
     * this section used to open with an empty text area and an Add button
     * above the notes, and a Delete beside every one of them. */
    expect(markup).not.toContain('aria-label="Manual note"');
    expect(markup).not.toContain('aria-label="New manual note"');
    expect(markup).not.toContain(">Delete<");
    expect(markup).toContain(">Add a note<");
  });

  test("the field that steers the next pass is an offer, not a form", () => {
    const markup = insightsMarkup({});
    /* A meeting nobody has steered opens with one line at the notes card's
     * bottom edge, beside the one that adds a note. The pane it opens used to
     * be a whole second card with a text area, a template picker and a
     * Re-enhance button standing open above every note on the page — three
     * controls and a form on a surface somebody came back to read. */
    expect(markup).toContain(">Guide the summary…<");
    expect(markup).not.toContain('aria-label="Guide the summary"');
    expect(markup).not.toContain(">Re-enhance with my notes<");
  });

  test("processing in flight replaces generated notes with an honest wait", () => {
    const markup = insightsMarkup({
      snapshot: {
        ...SNAPSHOT,
        artifacts: [],
        session: {
          ...SNAPSHOT.session,
          phase: "processing",
          processing_status: { kind: "running" },
        },
      },
    });
    expect(markup).toContain("Sona is still processing this meeting");
    expect(markup).toContain(">Refresh<");
  });

  /* A meeting with no notes has two genuinely different causes, and the screen
   * has to say which. This used to assert the opposite for four of the five:
   * only `remote_unavailable` was matched, so `local_model_unavailable` — the
   * reason `generation_shortfall` names "the case that filled the corpus with
   * blank meetings" — rendered the same "they can be rebuilt at any time"
   * screen as a meeting that recorded silence and has nothing to rebuild from.
   *
   * The headings are `meetings.processing.failed.*`, which the status chip
   * already reads, so a reason cannot be named here and left blank there. */
  /* One table for the whole union, checked against `ProcessingFailure`
   * itself: a sixth reason added to the union fails to compile here until it
   * is named, which is what makes this a matrix rather than five hand-copied
   * cases. `settingsRoute` carries the partition the pane draws — a reason a
   * person can act on in Settings keeps the route there, the rest do not — so
   * the split has one owner in this file as well. */
  const GENERATION_FAILURES = {
    local_model_unavailable: {
      heading: "Local model unavailable",
      settingsRoute: true,
    },
    remote_unavailable: {
      heading: "Remote destination unavailable",
      settingsRoute: true,
    },
    engine_failure: { heading: "Processing failed", settingsRoute: false },
    cancelled: { heading: "Processing cancelled", settingsRoute: false },
    interrupted: {
      heading: "Interrupted before it finished",
      settingsRoute: false,
    },
  } satisfies Record<
    ProcessingFailure,
    { heading: string; settingsRoute: boolean }
  >;

  // SAFETY: the table above satisfies `Record<ProcessingFailure, …>`, so its
  // own keys are exactly the members of that union.
  const reasons = Object.keys(GENERATION_FAILURES) as ProcessingFailure[];

  const failedMarkup = (reason: ProcessingFailure) =>
    insightsMarkup({
      snapshot: {
        ...SNAPSHOT,
        artifacts: [],
        session: {
          ...SNAPSHOT.session,
          processing_status: { kind: "failed", reason },
        },
      },
    });

  for (const reason of reasons) {
    test(
      "no notes because of " +
        reason +
        " names that reason and does not offer a rebuild",
      () => {
        const markup = failedMarkup(reason);
        expect(markup).toContain(GENERATION_FAILURES[reason].heading);
        expect(markup).toContain(
          "The transcript and your manual notes are unaffected.",
        );
        expect(markup).not.toContain("they can be rebuilt at any time");
      },
    );
  }

  for (const reason of reasons.filter(
    (candidate) => GENERATION_FAILURES[candidate].settingsRoute,
  )) {
    test(
      "no notes because of " +
        reason +
        " keeps Regenerate available and links to Settings",
      () => {
        const markup = failedMarkup(reason);
        expect(markup).toContain(GENERATION_FAILURES[reason].heading);
        expect(buttonTag(markup, "Settings")).not.toBe("");
        expect(buttonTag(markup, "Settings")).not.toMatch(
          /\sdisabled(?:="")?(?=\s|>)/,
        );
        expect(buttonTag(markup, "Regenerate")).not.toMatch(
          /\sdisabled(?:="")?(?=\s|>)/,
        );
      },
    );
  }

  for (const reason of reasons.filter(
    (candidate) => !GENERATION_FAILURES[candidate].settingsRoute,
  )) {
    test("no notes because of " + reason + " offers no Settings route", () => {
      expect(buttonTag(failedMarkup(reason), "Settings")).toBe("");
    });
  }

  /* The discriminator the negative assertion above rests on: a pass that
   * finished with nothing to say still offers the rebuild, so "not.toContain"
   * is a difference between two states rather than a string nothing renders. */
  test("a finished pass with nothing to say still offers the rebuild", () => {
    const markup = insightsMarkup({
      snapshot: {
        ...SNAPSHOT,
        artifacts: [],
        session: {
          ...SNAPSHOT.session,
          processing_status: { kind: "succeeded" },
        },
      },
    });
    expect(markup).toContain("No generated notes are available yet.");
    expect(markup).toContain("they can be rebuilt at any time");
  });
});

/* The ledger is the one surface where an inferred claim and the quote it was
 * read from have to stay side by side. These pin that: a state never renders
 * without its receipt, the receipt jumps to the transcript, and a failed
 * receipt check is named with counts rather than softened. */

const RECEIPT_CITATIONS = [
  {
    segment_id: "segment-1",
    start_offset_ns: 12_000_000_000,
    end_offset_ns: 14_000_000_000,
  },
];

const LEDGER: MeetingLedger = {
  headline: "Pricing came back at the end and nobody closed it.",
  threads: [
    {
      topic: "Pricing tiers",
      state: "unanswered",
      substantive: true,
      receipt: {
        quote: "We never actually said which tier the trial converts into.",
        speaker: "Dana",
        t_ms: 12_000,
        citations: RECEIPT_CITATIONS,
      },
      owner: null,
    },
    {
      topic: "Sign-off",
      state: "closed",
      substantive: false,
      receipt: {
        quote: "Right, that is everyone, thanks all.",
        speaker: "Amir",
        t_ms: 12_000,
        citations: RECEIPT_CITATIONS,
      },
      owner: "Amir",
    },
  ],
  open_loops: [
    {
      question: "Which tier does the trial convert into?",
      instead: "Amir answered the discount question instead.",
      at_ms: 12_000,
      citations: RECEIPT_CITATIONS,
    },
  ],
  commitments: [
    {
      who: "Amir",
      what: "Draft the tier comparison",
      firmness: "firm",
      receipt: {
        quote: "I will draft the tier comparison by Friday.",
        speaker: "Amir",
        t_ms: 12_000,
        citations: RECEIPT_CITATIONS,
      },
    },
  ],
  stances: [
    {
      from: "Amir",
      to: "Dana",
      what: "Ship the annual plan first",
      note: "Dana agreed without pushback.",
      at_ms: 12_000,
      citations: RECEIPT_CITATIONS,
    },
  ],
  caveats: [
    "Speaker labels came from diarization, not from names anyone said.",
  ],
  receipts: { status: "verified" },
};

/* The actionable half of the same ledger: the words come from the artifact
 * above, the state comes from the store. The question arrives carried forward,
 * which is the one row shape only a series produces. */
const LOOPS: MeetingLoopRow[] = [
  {
    loop_id: "session-1:loop:0f1e2d3c4b5a6978",
    session_id: "session-1",
    kind: "loop",
    text: "Which tier does the trial convert into?",
    owner_text: null,
    owner_person_id: null,
    owner_display_name: null,
    direction: "unattributed",
    status: "open",
    resolved_at_utc_ms: null,
    resolving_operation_id: null,
    carried_into_loop_id: null,
    carried_since_at_utc_ms: 1_700_000_000_000,
    at_ms: 12_000,
    revision: 0,
    instead: "Amir answered the discount question instead.",
    firmness: null,
    quote: null,
    speaker: null,
    citations: RECEIPT_CITATIONS,
  },
  {
    loop_id: "session-1:commitment:8796a5b4c3d2e1f0",
    session_id: "session-1",
    kind: "commitment",
    text: "Draft the tier comparison",
    owner_text: "Amir",
    owner_person_id: null,
    owner_display_name: null,
    direction: "waiting_on",
    status: "open",
    resolved_at_utc_ms: null,
    resolving_operation_id: null,
    carried_into_loop_id: null,
    carried_since_at_utc_ms: null,
    at_ms: 12_000,
    revision: 0,
    instead: null,
    firmness: "firm",
    quote: "I will draft the tier comparison by Friday.",
    speaker: "Amir",
    citations: RECEIPT_CITATIONS,
  },
];

/* bindings.ts is regenerated at integration, so a fixture attaches the ledger
 * the same way the app reads it: through one cast at the seam. */
const ledgerSnapshot = (ledger: MeetingLedger): MeetingReviewSnapshot => {
  const [artifact] = SNAPSHOT.artifacts;
  return {
    ...SNAPSHOT,
    artifacts: [
      {
        ...artifact,
        content: artifact.content && { ...artifact.content, ledger },
      },
    ],
  };
};

describe("meeting ledger", () => {
  const ledgerMarkup = (
    overrides: Partial<React.ComponentProps<typeof MeetingLedgerSection>>,
  ) =>
    render(
      <MeetingLedgerSection
        snapshot={ledgerSnapshot(LEDGER)}
        busy={false}
        canExport
        loops={LOOPS}
        people={[]}
        onJumpToSegment={noop}
        onExportLedger={noop}
        onLoopChange={noop}
        {...overrides}
      />,
    );

  test("states each thread beside the quote it was read from, and jumps to it", () => {
    const markup = ledgerMarkup({});
    expect(markup).toContain("Pricing tiers");
    expect(markup).toContain("No reply");
    expect(markup).toContain(
      "We never actually said which tier the trial converts into.",
    );
    /* The receipt is a citation, and a citation is a jump: the mark prints
     * the moment, and the word it used to print on every one of them is what
     * a reader who cannot see it hears. */
    expect(markup).toContain(">0:12</button>");
    expect(markup).toContain('aria-label="Transcript 0:12"');
    // Small talk stays on the record and out of the score: the sign-off is
    // `closed`, which is a landed state, and the score is still 0 of 1 — one
    // substantive thread, unanswered — rather than 1 of 2.
    expect(markup).toContain("Sign-off");
    expect(markup).toContain(">aside<");
    expect(markup).toContain(">0/1<");
  });

  test("carries the four registers and the receipt verdict", () => {
    const markup = ledgerMarkup({});
    expect(markup).toContain("Which tier does the trial convert into?");
    expect(markup).toContain("Amir answered the discount question instead.");
    expect(markup).toContain("Draft the tier comparison");
    expect(markup).toContain(">Firm<");
    expect(markup).toContain("Amir → Dana");
    expect(markup).toContain("Ship the annual plan first");
    expect(markup).toContain(
      "Speaker labels came from diarization, not from names anyone said.",
    );
    expect(markup).toContain(">verified<");
  });

  test("names what a failed receipt check removed, with counts", () => {
    const markup = ledgerMarkup({
      snapshot: ledgerSnapshot({
        ...LEDGER,
        receipts: {
          status: "degraded",
          dropped_threads: 2,
          dropped_commitments: 1,
        },
      }),
    });
    expect(markup).toContain("2 threads and 1 commitments removed");
  });

  test("offers the export, and no dead control when there is nothing to export", () => {
    expect(ledgerMarkup({})).toContain(">Export ledger page<");
    expect(
      buttonTag(ledgerMarkup({ canExport: false }), "Export ledger page"),
    ).toContain("disabled");

    const withoutLedger = ledgerMarkup({
      snapshot: { ...SNAPSHOT, artifacts: [] },
    });
    expect(withoutLedger).toContain(
      "No ledger has been read from this meeting yet",
    );
    expect(withoutLedger).not.toContain("Export ledger page");
  });

  test("reads the ledger off the newest current revision that carries one", () => {
    const [artifact] = ledgerSnapshot(LEDGER).artifacts;
    const stale = { ...artifact, state: "out_of_date" as const };
    const ledgerless = { ...SNAPSHOT.artifacts[0], artifact_id: "artifact-0" };
    expect(currentLedger([stale])).toBeNull();
    expect(currentLedger([ledgerless])).toBeNull();
    expect(currentLedger([ledgerless, artifact])?.ledger.headline).toBe(
      LEDGER.headline,
    );
  });

  /* D18: the two actionable registers are rows you act on, so each states
   * where it stands, who it belongs to, and offers only the controls its own
   * state can reach. A carried row says so — that line is the only thing on
   * the review screen that names the series. */
  test("states each loop, its owner, and only the controls its state allows", () => {
    const markup = ledgerMarkup({});

    expect(occurrences(markup, 'data-slot="loop-row"')).toBe(2);
    expect(occurrences(markup, ">Open<")).toBe(2);
    expect(markup).toContain("Owner: Nobody yet");
    expect(markup).toContain("Owner: Amir");
    expect(markup).toContain("Carried forward from an earlier meeting");
    // Open rows can be dropped; nothing here has been dropped or carried, so
    // there is no Reopen to press.
    expect(occurrences(markup, ">Drop</button>")).toBe(2);
    expect(markup).not.toContain(">Reopen</button>");
  });

  test("a settled loop offers the way back, and a done one is struck through", () => {
    const markup = ledgerMarkup({
      loops: [
        { ...LOOPS[0], status: "carried" },
        { ...LOOPS[1], status: "done" },
      ],
    });

    expect(markup).toContain(">Carried forward<");
    expect(markup).toContain(">Done<");
    expect(occurrences(markup, ">Reopen</button>")).toBe(1);
    expect(markup).not.toContain(">Drop</button>");
    expect(markup).toContain("line-through");
  });

  /* Until the first read lands there is nothing to act on: the registers read
   * as empty and every control is out, rather than a spinner over four rows. */
  test("reads as empty and inert before the first loop read lands", () => {
    const markup = ledgerMarkup({ loops: null });

    expect(markup).not.toContain('data-slot="loop-row"');
    expect(markup).toContain("No question was left without an answer.");
    expect(markup).toContain("Nobody committed to anything out loud.");
  });
});

/* What a row says about itself, and the one action that can change it.
 *
 * A meeting left behind by a launch that ended used to read "Processing"
 * forever: its phase was parked for a human but its recorded status still said
 * `pending`, and a chip that trusted the status could not tell the difference
 * between work in flight and work abandoned. The store now resolves that at
 * startup, and these tests hold the reading end of the same contract: no
 * status value on its own may produce the Processing chip, a row that ended
 * badly says why, and Retry appears only where it can run. */

const PHASES: MeetingPhase[] = [
  "preflight",
  "starting",
  "capturing_recording",
  "capturing_pausing",
  "capturing_paused",
  "capturing_resuming",
  "stopping",
  "processing",
  "review_ready",
  "recovery_required",
  "deleting",
];

const STATUSES: ProcessingStatus[] = [
  { kind: "pending" },
  { kind: "running" },
  { kind: "succeeded" },
  { kind: "cancelled" },
  { kind: "failed", reason: "interrupted" },
  { kind: "failed", reason: "engine_failure" },
  { kind: "failed", reason: "local_model_unavailable" },
];

/** The store's own list filters, as SQL-free predicates. Mirrors
 *  `status_predicate` in src-tauri/src/meeting/store.rs — the chip and the
 *  filter that returns a row have to agree, and this is where drift shows. */
const matchingFilters = (phase: MeetingPhase, status: ProcessingStatus) => {
  const arms: MeetingStatusFilter[] = ["any"];
  if (phase === "review_ready" && status.kind === "succeeded") {
    arms.push("ready");
  }
  if (
    phase === "processing" ||
    phase === "stopping" ||
    status.kind === "pending" ||
    status.kind === "running"
  ) {
    arms.push("processing");
  }
  if (
    phase === "recovery_required" ||
    status.kind === "failed" ||
    status.kind === "cancelled"
  ) {
    arms.push("failed");
  }
  return arms;
};

const STRANDED: MeetingHistorySummary = {
  ...SUMMARY,
  session_id: "meeting-stranded",
  title: "Yesterday's standup",
  phase: "recovery_required",
  processing_status: { kind: "failed", reason: "interrupted" },
};

describe("what a meeting row says about itself", () => {
  /** The phases a live job can belong to: the only ones the Processing chip is
   *  allowed to come from, whatever a row's status says. */
  const IN_FLIGHT: MeetingPhase[] = [
    "preflight",
    "stopping",
    "processing",
    "deleting",
  ];

  test("no processing status on its own can claim a meeting is processing", () => {
    for (const phase of PHASES) {
      for (const status of STATUSES) {
        if (meetingCardStatus(phase, status) !== "processing") continue;
        expect({
          phase,
          status: status.kind,
          inFlight: IN_FLIGHT.includes(phase),
        }).toEqual({ phase, status: status.kind, inFlight: true });
      }
    }
  });

  test("every shape the store can hold agrees with the filter that returns it", () => {
    /* The shapes a launch can leave on disk. A meeting is born `pending` in
     * preflight and stays `pending` while it captures; only a finished job
     * writes a terminal status, and startup reconciliation writes one for the
     * job nobody finished. That is why `review_ready` with an unresolved
     * status is not here — it would mean a meeting arrived at review without
     * its run ending — and why `deleting` is not either: the store's list
     * excludes that phase outright. */
    const reachable: [MeetingPhase, ProcessingStatus][] = [
      ["preflight", { kind: "pending" }],
      ["starting", { kind: "pending" }],
      ["capturing_recording", { kind: "pending" }],
      ["capturing_pausing", { kind: "pending" }],
      ["capturing_paused", { kind: "pending" }],
      ["capturing_resuming", { kind: "pending" }],
      ["stopping", { kind: "pending" }],
      ["processing", { kind: "pending" }],
      ["processing", { kind: "succeeded" }],
      ["processing", { kind: "cancelled" }],
      ["processing", { kind: "failed", reason: "engine_failure" }],
      ["review_ready", { kind: "succeeded" }],
      ["review_ready", { kind: "cancelled" }],
      ["review_ready", { kind: "failed", reason: "local_model_unavailable" }],
      // Before the sweep runs, and after it.
      ["recovery_required", { kind: "pending" }],
      ["recovery_required", { kind: "failed", reason: "interrupted" }],
      ["recovery_required", { kind: "failed", reason: "engine_failure" }],
    ];

    for (const [phase, status] of reachable) {
      const arms = matchingFilters(phase, status);
      const chip = meetingCardStatus(phase, status);
      expect({ phase, status, failed: chip === "needs_attention" }).toEqual({
        phase,
        status,
        failed: arms.includes("failed"),
      });
      expect({ phase, status, ready: chip === "ready" }).toEqual({
        phase,
        status,
        ready: arms.includes("ready"),
      });
      if (chip === "processing") {
        expect({
          phase,
          status,
          processingArm: arms.includes("processing"),
        }).toEqual({ phase, status, processingArm: true });
      }
    }
  });

  test("a meeting an interrupted launch left behind reads as needing attention", () => {
    expect(meetingCardStatus("recovery_required", { kind: "pending" })).toBe(
      "needs_attention",
    );
    expect(
      meetingCardStatus("recovery_required", {
        kind: "failed",
        reason: "interrupted",
      }),
    ).toBe("needs_attention");
    // The shape the sweep produces has left the Processing filter for good.
    expect(
      matchingFilters("recovery_required", {
        kind: "failed",
        reason: "interrupted",
      }),
    ).not.toContain("processing");
  });
});

describe("recovering a meeting from the list", () => {
  test("the row says what happened and offers to run it again", () => {
    const markup = homeMarkup({ meetings: [STRANDED] });
    expect(markup).toContain(">Needs attention</span>");
    expect(markup).toContain(">Interrupted before it finished</span>");
    // "Needs attention" is a state, not an explanation, so the reason has to
    // be on the row beside it.
    expect(occurrences(markup, ">Try again</button>")).toBe(1);
    expect(markup).not.toContain(">Processing</span>");
  });

  test("a finished meeting gets no chip, no reason line and no retry", () => {
    const markup = homeMarkup({ meetings: [SUMMARY] });
    expect(markup).toContain(">Weekly planning</span>");
    expect(markup).not.toContain(">Needs attention</span>");
    expect(markup).not.toContain(">Ready</span>");
    expect(occurrences(markup, ">Try again</button>")).toBe(0);
  });

  test("a meeting that failed after review keeps its reason and is offered no retry", () => {
    const markup = homeMarkup({
      meetings: [
        {
          ...SUMMARY,
          phase: "review_ready",
          processing_status: {
            kind: "failed",
            reason: "local_model_unavailable",
          },
        },
      ],
    });
    expect(markup).toContain(">Local model unavailable</span>");
    expect(occurrences(markup, ">Try again</button>")).toBe(
      0,
      // There is no command that reprocesses a meeting from review, so an
      // enabled Retry here would be a button that cannot do its job.
    );
  });
});

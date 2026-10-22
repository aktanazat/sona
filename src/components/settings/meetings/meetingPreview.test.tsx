import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type {
  MeetingReviewSnapshot,
  PersonBriefingRow,
  SourceKind,
} from "@/bindings";
import { formatDurationShort, formatEntryTimestamp } from "@/lib/utils/format";
import {
  MeetingPreviewCard,
  eventFacts,
  type MeetingPreviewCardProps,
  type MeetingPreviewFacts,
} from "./MeetingPreviewCard";
import { MeetingStartGate } from "./MeetingStartGate";
import type { CalendarEventSummary } from "./detectionStore";
import type { MeetingStartOptions } from "./meetingTypes";

/* Previewed meeting data, capture consent and accessible controls.
 * Static rendering runs no effects or handlers. */

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

const catalogue = JSON.parse(fs.readFileSync(localeRoot, "utf8"));

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

const occurrences = (markup: string, needle: string) =>
  markup.split(needle).length - 1;

/** The bound t every facts constructor takes, so a fallback title resolves
 * through the same catalogue the cards render with. */
const tr = i18n.t.bind(i18n);

const START = Date.UTC(2026, 7, 28, 17, 0);
const END = Date.UTC(2026, 7, 28, 17, 45);

/** A calendar event that carries everything EventKit can supply, so a test can
 * take facts away one at a time. Five participants, three of them named. */
const EVENT: CalendarEventSummary = {
  eventKey: "event-1",
  seriesKey: "series-1",
  title: "Quarterly planning",
  attendeeCount: 5,
  startUtcMs: START,
  endUtcMs: END,
  attendees: [
    { name: "Aktan Azat", status: "accepted", isSelf: true },
    { name: "Dana Reyes", status: "declined", isSelf: false },
    { name: "Sam Okafor", status: "pending", isSelf: false },
  ],
  notes: "Agenda: ship the preview card, then the ledger.",
  calendarName: "Work",
  url: "https://zoom.us/j/123456789",
};

/** An event from a calendar that told Sona the bare minimum. */
const BARE_EVENT: CalendarEventSummary = {
  eventKey: "event-2",
  seriesKey: "",
  title: "Focus block",
  attendeeCount: 0,
  startUtcMs: START,
  endUtcMs: END,
  attendees: [],
  notes: null,
  calendarName: null,
  url: null,
};

const BRIEFING: PersonBriefingRow[] = [
  {
    person_id: "person-1",
    display_name: "Dana Reyes",
    meetings_count: 3,
    last: {
      id: "meeting-1",
      title: "Planning",
      at_utc_ms: START - 86_400_000,
      headline: "Pricing remained open.",
    },
    open_loops: [
      {
        loop_id: "meeting-1:loop:a1b2c3d4e5f60718",
        meeting_id: "meeting-1",
        title: "Planning",
        at_utc_ms: START - 86_400_000,
        text: "Who owns the launch checklist?",
        owner_person_id: "person-1",
        status: "open",
        direction: "waiting_on",
        waiting_on_stale: false,
        carried_since_at_utc_ms: null,
        carried_into_meeting_id: null,
      },
    ],
    commitments: [],
  },
];

const card = (props: Partial<MeetingPreviewCardProps> = {}) =>
  render(
    <ul>
      <MeetingPreviewCard facts={eventFacts(EVENT, tr)} {...props} />
    </ul>,
  );

describe("collapse", () => {
  test("a card arrives collapsed and says so to assistive tech", () => {
    const markup = card();

    expect(markup).toContain('aria-expanded="false"');
  });
  test("an expanded card reports the open state on both halves", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain('aria-expanded="true"');
  });
});

describe("the collapsed row", () => {
  test("carries the start, the duration and the head count", () => {
    const markup = card();

    expect(markup).toContain(formatEntryTimestamp(START));
    expect(markup).toContain(formatDurationShort((END - START) / 1000));
    expect(markup).toContain("5 people");
  });

  test("prints a live countdown only while one is running", () => {
    expect(card({ secondsToStart: 45 })).toContain("Starts in 45s");
    expect(card()).not.toContain("Starts in");
  });

  test("the countdown survives the disclosure, because no row carries it", () => {
    const open = card({ secondsToStart: 45, defaultExpanded: true });

    expect(occurrences(open, "Starts in 45s")).toBe(1);
  });

  test("shows the frozen relationship briefing, and only where one was given", () => {
    const markup = card({ briefing: BRIEFING });

    expect(markup).toContain("You have met Dana Reyes 3 times");
    expect(markup).toContain("Who owns the launch checklist?");
    expect(markup).toContain(formatEntryTimestamp(START - 86_400_000));
  });

  test("omits the head count for an event with no attendee list", () => {
    const markup = render(
      <ul>
        <MeetingPreviewCard facts={eventFacts(BARE_EVENT, tr)} />
      </ul>,
    );

    expect(markup).not.toContain("people");
    expect(markup).not.toContain("person");
  });
});

describe("participants", () => {
  test("each named person carries their answer as a glyph and as words", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain("Aktan Azat");
    expect(markup).toContain("Dana Reyes");
    expect(markup).toContain("Sam Okafor");
    // The tally is the text half: a glyph never carries the state alone.
    expect(markup).toContain("Accepted 1");
    expect(markup).toContain("Declined 1");
    expect(markup).toContain("No reply yet 1");
  });

  test("the people EventKit would not name are counted, never invented", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain("2 more, not named");
  });
});

describe("description", () => {
  test("shows the event's own notes behind a one-line disclosure", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain("Agenda: ship the preview card, then the ledger.");
    expect(markup).toContain(">More</button>");
  });
});

describe("recording", () => {
  const armed = (sources: SourceKind[]) =>
    card({
      defaultExpanded: true,
      recording: {
        armed: sources,
        onToggle: () => {},
        disabled: false,
      },
    });

  test("both sources armed reads as two pressed chips", () => {
    const markup = armed(["microphone", "system_audio"]);

    expect(occurrences(markup, 'aria-pressed="true"')).toBe(2);
    expect(occurrences(markup, 'aria-pressed="false"')).toBe(0);
  });

  test("nothing armed still shows both chips, unpressed", () => {
    const markup = armed([]);

    expect(occurrences(markup, 'aria-pressed="true"')).toBe(0);
    expect(occurrences(markup, 'aria-pressed="false"')).toBe(2);
  });

  test("settled sources read as text, because the preflight cannot re-arm them", () => {
    const markup = card({
      defaultExpanded: true,
      recording: { armed: ["microphone"] },
    });

    expect(markup).toContain("Microphone");
    expect(markup).not.toContain("aria-pressed");
  });

  test("a settled start with no source says so instead of showing an empty row", () => {
    const markup = card({ defaultExpanded: true, recording: { armed: [] } });

    expect(markup).toContain("No source chosen");
  });
});

describe("notify", () => {
  test("the auto-open switch reflects the real setting on both sides", () => {
    const on = card({
      defaultExpanded: true,
      notify: {
        access: "authorized",
        delivered: null,
        autoOpen: { checked: true, onChange: () => {}, disabled: false },
      },
    });
    const off = card({
      defaultExpanded: true,
      notify: {
        access: "authorized",
        delivered: null,
        autoOpen: { checked: false, onChange: () => {}, disabled: false },
      },
    });

    /* Radix's switch is a `role="switch"` button, not a checkbox, and its
     * accessible name is what a screen reader reports for the setting — so the
     * name is asserted here rather than left to the visual row label, which
     * this control does not have. */
    expect(on).toContain('aria-checked="true"');
    expect(on).toContain('aria-label="Open this meeting when it starts"');
    expect(off).toContain('aria-checked="false"');
    expect(occurrences(off, 'role="switch"')).toBe(1);
  });
});

describe("notes", () => {
  test("shows the shape the generated notes will take", () => {
    const markup = card({ defaultExpanded: true, notesTemplate: "one_on_one" });

    expect(markup).toContain(i18n.t("meetings.notes.templates.one_on_one"));
  });
});

describe("link", () => {
  test("the event's own URL is a control, labelled by its host", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain(">zoom.us</button>");
  });
});

describe("the header never renders blank", () => {
  const blank = (facts: Partial<MeetingPreviewFacts>) =>
    render(
      <ul>
        <MeetingPreviewCard
          facts={{ ...eventFacts(BARE_EVENT, tr), title: "", ...facts }}
        />
      </ul>,
    );
  const heading = (markup: string) => {
    const at = markup.indexOf('data-slot="preview-title"');
    if (at === -1) return undefined;
    const open = markup.indexOf(">", at);
    return markup.slice(open + 1, markup.indexOf("</span>", open));
  };

  test("bad facts fall back to the app name before anything generic", () => {
    expect(heading(blank({ origin: "app", appName: "Zoom" }))).toBe("Zoom");
  });
});

describe("the action bar", () => {
  test("start reports the attempt already under way", () => {
    const markup = card({ onStart: () => {}, starting: true });

    expect(markup).toContain("Starting…");
    expect(markup).toMatch(/<button(?=[^>]*disabled="")[^>]*>Starting…<\/button>/);
  });

  test("skip appears only where a surface can honour it", () => {
    expect(card({ onSkip: () => {} })).toContain(">Skip</button>");
    expect(card({ onStart: () => {} })).not.toContain(">Skip</button>");
  });
});

/* A preflight snapshot as the backend produces one: the session exists, both
 * sources are available, and nothing has been captured yet. */
const PREFLIGHT: MeetingReviewSnapshot = {
  session: {
    session_id: "meeting-1",
    phase: "preflight",
    revision: 1,
    title: "Quarterly planning",
    started_at_utc_ms: null,
    elapsed_offset_ns: null,
    sources: [
      {
        track_id: null,
        source_kind: "microphone",
        required: true,
        availability: "available",
        health: "not_started",
        format: null,
        last_durable_offset_ns: null,
        gap_count: 0,
      },
      {
        track_id: null,
        source_kind: "system_audio",
        required: true,
        availability: "available",
        health: "not_started",
        format: null,
        last_durable_offset_ns: null,
        gap_count: 0,
      },
    ],
    open_capture_window_started_at_ns: null,
    capture_completeness: "not_started",
    storage: "available",
    processing_status: { kind: "pending" },
    retention_deadline_utc_ms: null,
    allowed_actions: [],
  },
  tracks: [],
  gaps: [],
  speakers: [],
  transcript: [],
  notes: [],
  artifacts: [],
  questions: [],
  diarization: {
    status: "not_requested",
    model_id: "diarizer",
    model_version: "1",
    generation_id: null,
    assigned_segment_count: 0,
  },
  can_export: false,
  remote_cancellation_pending: false,
};

const GATE_OPTIONS: MeetingStartOptions = {
  title: "Quarterly planning",
  origin: "manual",
  suggestionId: null,
  calendarEventKey: EVENT.eventKey,
  sources: ["microphone", "system_audio"],
  degradedStartPolicy: "abort_if_required_source_fails",
  destination: { kind: "local" },
  preview: eventFacts(EVENT, tr),
};

describe("the preflight", () => {
  const gate = (options: MeetingStartOptions) =>
    render(
      <MeetingStartGate
        snapshot={PREFLIGHT}
        options={options}
        refreshing={false}
        starting={false}
        onRefresh={() => {}}
        onCancel={() => {}}
        onStart={() => {}}
      />,
    );

  test("previews the meeting the operator was looking at", () => {
    const markup = gate(GATE_OPTIONS);

    expect(markup).toContain("Quarterly planning");
    expect(markup).toContain("Aktan Azat");
  });

  test("the preview adds no second Start, so consent stays one press", () => {
    const label = `>${i18n.t("meetings.start.action")}</button>`;

    /* The gate's own Start is the act the backend records as consent. A card
     * that shipped a second one would make it ambiguous which press was
     * acknowledged, which is exactly the thing the consent receipt exists to
     * pin down. */
    expect(occurrences(gate(GATE_OPTIONS), label)).toBe(1);
    expect(occurrences(gate({ ...GATE_OPTIONS, preview: null }), label)).toBe(
      1,
    );
  });
});

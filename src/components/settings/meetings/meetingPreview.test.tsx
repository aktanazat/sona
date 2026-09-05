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
  MeetingSuggestion,
  PersonBriefingRow,
  SourceKind,
} from "@/bindings";
import { formatDurationShort, formatEntryTimestamp } from "@/lib/utils/format";
import {
  MeetingPreviewCard,
  eventFacts,
  suggestionFacts,
  type MeetingPreviewCardProps,
  type MeetingPreviewFacts,
} from "./MeetingPreviewCard";
import { MeetingStartGate } from "./MeetingStartGate";
import type { CalendarEventSummary } from "./detectionStore";
import type { MeetingStartOptions } from "./meetingTypes";

/* What this file defends: the card never invents a row.
 *
 * The whole point of the preview is that the operator can read what Sona knows
 * about a call before recording it. A row that appears with a blank, a dash or
 * a plausible guess in it teaches them the opposite lesson, so the matrix below
 * renders the same card with each fact present and then absent, and asserts the
 * row disappears rather than empties.
 *
 * Static rendering runs no effects and no handlers, so these are prop-to-markup
 * checks: nothing here can reach a Tauri command. The collapse is CSS, which is
 * why "collapsed" is asserted as `data-open="false"` with the content still in
 * the markup — content that vanished on collapse could not animate out. */

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

/** Every row label the card can print, so "no invented rows" can be asserted
 * as a set rather than one label at a time. */
const ROW_LABELS = [
  "Time",
  "Calendar",
  "App",
  "Notify",
  "Recording",
  "Notes",
  "Participants",
  "Link",
  "Description",
] as const;

const rowLabel = (label: string) => `>${label}</span>`;

describe("collapse", () => {
  test("a card arrives collapsed and says so to assistive tech", () => {
    const markup = card();

    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain('data-open="false"');
  });

  test("collapsed content stays in the markup, so it has something to animate", () => {
    const markup = card();

    /* The body is hidden by grid track and visibility, never by removal: a row
     * that only existed once expanded would pop in at the end of the 120ms
     * curve instead of sliding with it. */
    expect(markup).toContain("Quarterly planning");
    expect(markup).toContain("Work");
    expect(markup).toContain("Agenda: ship the preview card");
  });

  test("an expanded card reports the open state on both halves", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain('aria-expanded="true"');
    expect(markup).toContain('data-open="true"');
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

  test("the head stops repeating the rows once the rows are showing", () => {
    /* Collapsed, the start is in the head chip and in the TIME row the
     * disclosure is hiding, so it appears twice in the markup. Open, the row
     * is the only copy: the summary was crowding the title into an ellipsis to
     * make room for its own echo. */
    const start = formatEntryTimestamp(START);

    expect(occurrences(card(), start)).toBe(2);
    expect(occurrences(card({ defaultExpanded: true }), start)).toBe(1);
  });

  test("the countdown survives the disclosure, because no row carries it", () => {
    const open = card({ secondsToStart: 45, defaultExpanded: true });

    expect(occurrences(open, "Starts in 45s")).toBe(1);
  });

  test("shows the frozen relationship briefing, and only where one was given", () => {
    const markup = card({ briefing: BRIEFING });

    expect(markup).toContain('data-slot="preview-briefing"');
    expect(markup).toContain("You have met Dana Reyes 3 times");
    expect(markup).toContain("Who owns the launch checklist?");
    expect(markup).toContain(formatEntryTimestamp(START - 86_400_000));
    /* The briefing belongs to the people slice, so its own markup is not
     * pinned here — counting its paragraph classes made this card's test fail
     * on a restyle two folders away that changed nothing this card owns. */
    expect(card()).not.toContain('data-slot="preview-briefing"');
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

    expect(markup).toContain(rowLabel("Participants"));
    expect(markup).toContain("Aktan Azat");
    expect(markup).toContain("Dana Reyes");
    expect(markup).toContain("Sam Okafor");
    // The tally is the text half: a glyph never carries the state alone.
    expect(markup).toContain("Accepted 1");
    expect(markup).toContain("Declined 1");
    expect(markup).toContain("No reply yet 1");
    expect(markup).toContain('data-status="declined"');
  });

  test("the people EventKit would not name are counted, never invented", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain("2 more, not named");
  });

  test("marks which of them is you", () => {
    expect(card({ defaultExpanded: true })).toContain("You");
  });

  test("an event with no named participants has no participants row", () => {
    const markup = render(
      <ul>
        <MeetingPreviewCard
          facts={eventFacts(BARE_EVENT, tr)}
          defaultExpanded
        />
      </ul>,
    );

    expect(markup).not.toContain(rowLabel("Participants"));
    expect(markup).not.toContain("not named");
  });
});

describe("description", () => {
  test("shows the event's own notes behind a one-line disclosure", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain(rowLabel("Description"));
    expect(markup).toContain("Agenda: ship the preview card, then the ledger.");
    expect(markup).toContain('data-slot="preview-description"');
    expect(markup).toContain(">More</button>");
  });

  test("an event with no notes has no description row", () => {
    const markup = render(
      <ul>
        <MeetingPreviewCard
          facts={eventFacts(BARE_EVENT, tr)}
          defaultExpanded
        />
      </ul>,
    );

    expect(markup).not.toContain(rowLabel("Description"));
    expect(markup).not.toContain(">More</button>");
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

    expect(markup).toContain(rowLabel("Recording"));
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

    expect(markup).toContain(rowLabel("Recording"));
    expect(markup).toContain("Microphone");
    expect(markup).not.toContain("aria-pressed");
  });

  test("a settled start with no source says so instead of showing an empty row", () => {
    const markup = card({ defaultExpanded: true, recording: { armed: [] } });

    expect(markup).toContain("No source chosen");
  });

  test("a surface that arms nothing has no recording row at all", () => {
    expect(card({ defaultExpanded: true })).not.toContain(
      rowLabel("Recording"),
    );
  });
});

describe("notify", () => {
  test("a delivered prompt says it was delivered", () => {
    const markup = card({
      defaultExpanded: true,
      notify: { access: "authorized", delivered: true, autoOpen: null },
    });

    expect(markup).toContain(rowLabel("Notify"));
    expect(markup).toContain("Notification sent");
  });

  test("denied notifications name the degradation instead of staying silent", () => {
    const markup = card({
      defaultExpanded: true,
      notify: { access: "denied", delivered: null, autoOpen: null },
    });

    expect(markup).toContain("Shown in Sona only");
  });

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
    expect(on).toContain('data-state="checked"');
    expect(on).toContain('aria-label="Open this meeting when it starts"');
    expect(off).toContain('aria-checked="false"');
    expect(off).toContain('data-state="unchecked"');
    expect(occurrences(off, 'role="switch"')).toBe(1);
  });
});

describe("notes", () => {
  test("shows the shape the generated notes will take", () => {
    const markup = card({ defaultExpanded: true, notesTemplate: "one_on_one" });

    expect(markup).toContain(rowLabel("Notes"));
    expect(markup).toContain(i18n.t("meetings.notes.templates.one_on_one"));
  });

  test("a surface that has not read the setting shows no notes row", () => {
    expect(card({ defaultExpanded: true })).not.toContain(rowLabel("Notes"));
  });
});

describe("link", () => {
  test("the event's own URL is a control, labelled by its host", () => {
    const markup = card({ defaultExpanded: true });

    expect(markup).toContain(rowLabel("Link"));
    expect(markup).toContain(">zoom.us</button>");
  });
});

describe("no invented rows", () => {
  test("an event that told Sona nothing but a title and a time prints two rows", () => {
    const markup = render(
      <ul>
        <MeetingPreviewCard
          facts={eventFacts(BARE_EVENT, tr)}
          defaultExpanded
        />
      </ul>,
    );
    const printed = ROW_LABELS.filter((label) =>
      markup.includes(rowLabel(label)),
    );

    expect(printed).toEqual(["Time"]);
    expect(markup.includes("__MISSING__")).toBe(false);
  });

  test("an offer from a running app prints only what an offer knows", () => {
    const markup = render(
      <ul>
        <MeetingPreviewCard
          facts={suggestionFacts(SUGGESTION, tr)}
          defaultExpanded
        />
      </ul>,
    );
    const printed = ROW_LABELS.filter((label) =>
      markup.includes(rowLabel(label)),
    );

    expect(printed).toEqual(["App"]);
    expect(markup).toContain("Zoom");
  });
});

describe("the head row", () => {
  test("the decision sits in the head, never in a band below the body", () => {
    const markup = card({ onStart: () => {}, onSkip: () => {} });

    /* Head first, actions inside it, body after: a collapsed card is exactly
     * one row, with no reserved blank space under it. */
    expect(occurrences(markup, 'data-slot="preview-head"')).toBe(1);
    expect(markup.indexOf('data-slot="preview-actions"')).toBeGreaterThan(-1);
    expect(markup.indexOf('data-slot="preview-actions"')).toBeLessThan(
      markup.indexOf('data-slot="preview-body"'),
    );
  });

  test("an offer's head carries no chips, because an offer measures nothing", () => {
    const markup = render(
      <ul>
        <MeetingPreviewCard facts={suggestionFacts(SUGGESTION, tr)} />
      </ul>,
    );

    /* No chips means no rail for chips: the container itself is absent rather
     * than present and empty. */
    expect(markup).not.toContain('data-slot="preview-facts"');
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

  test("an untitled calendar event is named for what it is", () => {
    expect(eventFacts({ ...BARE_EVENT, title: "  " }, tr).title).toBe(
      "Calendar event",
    );
  });

  test("bad facts fall back to the app name before anything generic", () => {
    expect(heading(blank({ origin: "app", appName: "Zoom" }))).toBe("Zoom");
  });

  test("bad facts with no app name still name the origin", () => {
    expect(heading(blank({ origin: "app" }))).toBe("Microphone in use");
    expect(heading(blank({ origin: "calendar" }))).toBe("Calendar event");
  });
});

describe("the action bar", () => {
  test("start carries the same label as the page's own start, not a new one", () => {
    const markup = card({ onStart: () => {} });

    /* One start on the card, and it is the page's own label rather than a
     * second vocabulary for the same act. */
    expect(
      occurrences(markup, `>${i18n.t("meetings.start.action")}</button>`),
    ).toBe(1);
  });

  test("start reports the attempt already under way", () => {
    const markup = card({ onStart: () => {}, starting: true });

    expect(markup).toContain("Starting…");
    expect(markup).toContain("disabled");
  });

  test("skip appears only where a surface can honour it", () => {
    expect(card({ onSkip: () => {} })).toContain(">Skip</button>");
    expect(card({ onStart: () => {} })).not.toContain(">Skip</button>");
  });

  test("a card with no actions renders no action bar", () => {
    expect(card()).not.toContain('data-slot="preview-actions"');
  });

  test("there is no yes/no/maybe: the card never answers the invitation", () => {
    const markup = card({ onStart: () => {}, onSkip: () => {} });

    expect(markup).not.toContain(">Yes</button>");
    expect(markup).not.toContain(">No</button>");
    expect(markup).not.toContain(">Maybe</button>");
  });
});

describe("facts from the backend", () => {
  test("a calendar event maps every field the calendar supplied", () => {
    expect(eventFacts(EVENT, tr)).toEqual({
      id: "event-1",
      title: "Quarterly planning",
      origin: "calendar",
      startUtcMs: START,
      endUtcMs: END,
      calendarName: "Work",
      appName: null,
      attendeeCount: 5,
      participants: [
        { name: "Aktan Azat", status: "accepted", isSelf: true },
        { name: "Dana Reyes", status: "declined", isSelf: false },
        { name: "Sam Okafor", status: "pending", isSelf: false },
      ],
      description: "Agenda: ship the preview card, then the ledger.",
      url: "https://zoom.us/j/123456789",
    });
  });

  test("what the calendar left empty stays empty", () => {
    const facts = eventFacts(BARE_EVENT, tr);

    expect(facts.calendarName).toBe(null);
    expect(facts.description).toBe(null);
    expect(facts.url).toBe(null);
    expect(facts.participants).toEqual([]);
  });

  test("an offer carries its app and nothing it never saw", () => {
    const facts = suggestionFacts(SUGGESTION, tr);

    expect(facts.origin).toBe("app");
    expect(facts.appName).toBe("Zoom");
    expect(facts.startUtcMs).toBe(null);
    expect(facts.endUtcMs).toBe(null);
    expect(facts.attendeeCount).toBe(null);
    expect(facts.description).toBe(null);
    expect(facts.url).toBe(null);
    expect(facts.participants).toEqual([]);
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

    expect(markup).toContain('data-slot="preview-summary"');
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

  test("a manual start with nothing to preview shows no card", () => {
    expect(gate({ ...GATE_OPTIONS, preview: null })).not.toContain(
      'data-slot="preview-summary"',
    );
  });
});

describe("english catalogue", () => {
  /* Every key the card references, so a rename breaks here rather than
   * silently degrading to the key itself at runtime. */
  const KEYS = [
    "meetings.preview.rows.time",
    "meetings.preview.rows.calendar",
    "meetings.preview.rows.app",
    "meetings.preview.rows.notify",
    "meetings.preview.rows.recording",
    "meetings.preview.rows.notes",
    "meetings.preview.rows.participants",
    "meetings.preview.rows.link",
    "meetings.preview.rows.description",
    "meetings.preview.attendees_one",
    "meetings.preview.attendees_other",
    "meetings.preview.notify.delivered",
    "meetings.preview.notify.willNotify",
    "meetings.preview.notify.inApp",
    "meetings.preview.notify.autoOpen",
    "meetings.preview.recording.none",
    "meetings.preview.participation.accepted",
    "meetings.preview.participation.declined",
    "meetings.preview.participation.tentative",
    "meetings.preview.participation.pending",
    "meetings.preview.participation.unknown",
    "meetings.preview.participation.you",
    "meetings.preview.participation.tally",
    "meetings.preview.participation.unnamed_one",
    "meetings.preview.participation.unnamed_other",
    "meetings.preview.description.more",
    "meetings.preview.description.less",
    "meetings.preview.actions.skip",
    "meetings.preview.linkFailed",
    "meetings.preview.untitled.calendar",
    "meetings.preview.untitled.app",
  ];

  for (const key of KEYS) {
    test(`${key} is translated`, () => {
      /* Resolved through the same i18next instance the cards render with, so
       * this asserts what a reader would actually see. A key with no entry
       * comes back as the __MISSING__ sentinel configured above; a key that
       * resolves to an object rather than copy comes back as the key. */
      const value = i18n.t(key);

      expect(value).not.toBe("__MISSING__");
      expect(value).not.toBe(key);
    });
  }
});

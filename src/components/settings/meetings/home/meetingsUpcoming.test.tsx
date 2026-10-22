import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import {
  commands,
  type CalendarAccess,
  type MeetingSeriesAlwaysRecordSetRequest,
  type MeetingSeriesDigestSetRequest,
  type MeetingSeriesMutationResult,
  type MeetingUpcomingEvents,
  type MeetingUpcomingRow,
} from "@/bindings";
import { MeetingsUpcomingView, SeriesControls } from "./MeetingsUpcoming";
import {
  useUpcomingEvents,
  type UpcomingEventsState,
} from "./useUpcomingEvents";


const localeRoot = path.join(
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

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localeRoot, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

const render = (node: React.ReactElement) =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const occurrences = (markup: string, needle: string) =>
  markup.split(needle).length - 1;

/* A Tuesday morning, so the day heading is a weekday rather than "Today" and
 * the grouping is visible in the markup. */
const DAY_ONE = new Date(2026, 8, 15, 9, 30).getTime();
const DAY_TWO = new Date(2026, 8, 16, 14, 0).getTime();
const HALF_HOUR = 30 * 60_000;

const RECURRING: MeetingUpcomingRow = {
  event_key: "weekly-sync#1",
  title: "Weekly sync",
  start_utc_ms: DAY_ONE,
  end_utc_ms: DAY_ONE + HALF_HOUR,
  attendees: [
    { name: "Steven", status: "accepted", is_self: false, person_id: "p-1" },
    { name: "Dana", status: "pending", is_self: false, person_id: null },
  ],
  attendee_count: 4,
  calendar_name: "Work",
  join_url: "https://meet.example.com/abc",
  series: {
    series_key: "weekly-sync",
    always_record: false,
    template: null,
    digest_included: true,
  },
};

const ONE_OFF: MeetingUpcomingRow = {
  event_key: "coffee#1",
  title: "Coffee with Dana",
  start_utc_ms: DAY_TWO,
  end_utc_ms: DAY_TWO + HALF_HOUR,
  attendees: [],
  attendee_count: 0,
  calendar_name: "Personal",
  join_url: null,
  series: null,
};

const events = (
  rows: MeetingUpcomingRow[],
  access: CalendarAccess = "authorized",
): MeetingUpcomingEvents => ({
  access,
  window_start_utc_ms: DAY_ONE,
  window_end_utc_ms: DAY_TWO,
  rows,
  series_revision: 4,
});

const noop = () => Promise.resolve();

const view = (
  props: Partial<React.ComponentProps<typeof MeetingsUpcomingView>> = {},
) =>
  render(
    <MeetingsUpcomingView
      events={events([RECURRING, ONE_OFF])}
      loading={false}
      saving={null}
      setAlwaysRecord={noop}
      setTemplate={noop}
      setDigestIncluded={noop}
      {...props}
    />,
  );

describe("Upcoming section", () => {
  test("groups rows by day and states time, title and calendar", () => {
    const markup = view();

    expect(occurrences(markup, 'data-slot="upcoming-day"')).toBe(2);
    expect(occurrences(markup, 'data-slot="upcoming-row"')).toBe(2);
    expect(markup).toContain("Weekly sync");
    expect(markup).toContain("Coffee with Dana");
    expect(markup).toContain("Work");
    expect(markup).toContain("Personal");
  });

  test("marks only the recurring row as a series and offers only it controls", () => {
    const markup = view();

    expect(occurrences(markup, "Series options for Weekly sync")).toBe(1);
    expect(markup).not.toContain("Series options for Coffee with Dana");
  });

  test("a calendar access notice offers no dead Settings control without a route", () => {
    const markup = view({ events: events([], "not_determined") });

    expect(markup).not.toContain("<button");
  });

  test("the fix is pressable where the shell can route to Settings", () => {
    const markup = view({
      events: events([], "not_determined"),
      onOpenSettings: () => {},
    });

    expect(markup).toMatch(
      /<button[^>]*>Turn on “Use my calendar” in Settings<\/button>/,
    );
  });

});

describe("Upcoming series controls, rendered", () => {
  const controls = (
    props: Partial<React.ComponentProps<typeof SeriesControls>> = {},
  ) =>
    render(
      <SeriesControls
        row={RECURRING}
        saving={false}
        onAlwaysRecord={() => {}}
        onTemplate={() => {}}
        onDigest={() => {}}
        {...props}
      />,
    );

  test("names the series recording, digest and template controls", () => {
    const markup = controls();

    expect(markup).toContain("Always record this series");
    expect(markup).toContain("Include in the evening digest");
    expect(occurrences(markup, 'aria-label="Notes template"')).toBe(1);
  });

  /* Each switch reflects what the series actually stored, independently. The
   * fixture below has always-record on and the digest off, which is the pair
   * that catches a component wiring both switches to one field.
   *
   * The picker's current label is deliberately not asserted: Radix resolves it
   * from the item that registers on mount, so a static render shows an empty
   * value node for any selection at all — an assertion here would pass for
   * "One-to-one" and for nothing with equal enthusiasm. */
  test("each switch reflects its own stored choice", () => {
    const markup = controls({
      row: {
        ...RECURRING,
        series: {
          series_key: "weekly-sync",
          always_record: true,
          template: "one_on_one",
          digest_included: false,
        },
      },
    });

    expect(occurrences(markup, 'aria-checked="true"')).toBe(1);
    expect(occurrences(markup, 'aria-checked="false"')).toBe(1);
    expect(markup.indexOf('aria-checked="true"')).toBeLessThan(
      markup.indexOf('aria-checked="false"'),
    );
  });

  test("a write in flight quiets the row's controls", () => {
    const markup = controls({ saving: true });
    expect(
      markup.match(/<button(?=[^>]*disabled="")[^>]*>/g) ?? [],
    ).toHaveLength(3);
  });

});

interface HookCapture {
  state: UpcomingEventsState | null;
}

/** Drives the hook once against a stubbed command surface. */
const driveHook = (): HookCapture => {
  const captured: HookCapture = { state: null };
  const Harness = () => {
    captured.state = useUpcomingEvents(["microphone", "system_audio"]);
    return null;
  };
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <Harness />
    </I18nextProvider>,
  );
  return captured;
};

const mutation = (
  preferences: MeetingSeriesMutationResult["preferences"],
): MeetingSeriesMutationResult => ({
  receipt: {
    schema_version: 1,
    operation_id: "op-1",
    session_id: null,
    actor: "user",
    command: "series_digest_set",
    expected_revision: 4,
    from_phase: null,
    to_phase: null,
    requested_at_utc_ms: 1,
    committed_at_utc_ms: 2,
    result: "committed",
    reason_codes: [],
    new_revision: 5,
    effect_ids: ["weekly-sync"],
  },
  preferences,
});

describe("Upcoming series controls", () => {
  /* The always-record toggle is the standing-consent mutation, not a new one,
   * and the grant it writes has to name the capture sources the operator can
   * see on this page. A grant naming nothing is a grant to record anything. */
  test("always record grants the standing consent with the visible sources", async () => {
    const original = commands.meetingSeriesAlwaysRecordSet;
    const requests = new Array<MeetingSeriesAlwaysRecordSetRequest>();
    commands.meetingSeriesAlwaysRecordSet = async (request) => {
      requests.push(request);
      return {
        status: "ok",
        data: mutation({
          series_key: "weekly-sync",
          template: null,
          digest_included: true,
          always_record: true,
          remote_intelligence_opt_out: false,
          announce_in_chat: false,
          revision: 5,
        }),
      };
    };
    const captured = driveHook();

    try {
      await captured.state?.setAlwaysRecord("weekly-sync", true);
      expect(requests.length).toBe(1);
      expect(requests[0]?.series_key).toBe("weekly-sync");
      expect(requests[0]?.always_record).toBe(true);
      expect(requests[0]?.acknowledged_sources).toEqual([
        "microphone",
        "system_audio",
      ]);
      expect(requests[0]?.policy_version).toBe(1);
    } finally {
      commands.meetingSeriesAlwaysRecordSet = original;
    }
  });

  /* Revoking needs no acknowledgement, and sending one would claim the
   * operator re-consented on the way out. */
  test("turning always record off acknowledges nothing", async () => {
    const original = commands.meetingSeriesAlwaysRecordSet;
    const requests = new Array<MeetingSeriesAlwaysRecordSetRequest>();
    commands.meetingSeriesAlwaysRecordSet = async (request) => {
      requests.push(request);
      return { status: "error", error: "invalid_request" };
    };
    const captured = driveHook();

    try {
      await captured.state?.setAlwaysRecord("weekly-sync", false);
      expect(requests[0]?.always_record).toBe(false);
      expect(requests[0]?.acknowledged_sources).toEqual([]);
    } finally {
      commands.meetingSeriesAlwaysRecordSet = original;
    }
  });

  test("digest inclusion is its own mutation, not part of the template write", async () => {
    const originalDigest = commands.meetingSeriesDigestSet;
    const originalTemplate = commands.meetingSeriesTemplateSet;
    const digestRequests = new Array<MeetingSeriesDigestSetRequest>();
    let templateCalls = 0;
    commands.meetingSeriesDigestSet = async (request) => {
      digestRequests.push(request);
      return { status: "error", error: "invalid_request" };
    };
    commands.meetingSeriesTemplateSet = async () => {
      templateCalls += 1;
      return { status: "error", error: "invalid_request" };
    };
    const captured = driveHook();

    try {
      await captured.state?.setDigestIncluded("weekly-sync", false);
      expect(digestRequests.length).toBe(1);
      expect(digestRequests[0]?.digest_included).toBe(false);
      expect(templateCalls).toBe(0);
    } finally {
      commands.meetingSeriesDigestSet = originalDigest;
      commands.meetingSeriesTemplateSet = originalTemplate;
    }
  });
});

import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type {
  Document,
  MeetingLedger,
  MeetingPersonContextRow,
  Person,
  PersonDetail,
  PersonListEntry,
  PersonMeetingLink,
} from "@/bindings";
import {
  buildFollowUpAgentMessage,
  type FollowUpAgentMessageSource,
} from "@/components/settings/meetings/review/FollowUpAgentAction";
import {
  PreviouslyTogetherBandView,
  previouslyTogetherRows,
} from "@/components/settings/meetings/review/PreviouslyTogetherBand";
import { OrganizationView } from "./OrganizationView";
import { PeopleListView } from "./PeopleList";
import { PersonDetailView } from "./PersonDetailView";
import { monthlyMeetingCadence } from "./peopleModel";

const localePath = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
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
const occurrences = (markup: string, needle: string) =>
  markup.split(needle).length - 1;
const noop = () => undefined;

const JANUARY = Date.UTC(2026, 0, 12, 17);
const FEBRUARY = Date.UTC(2026, 1, 4, 17);
const JUNE = Date.UTC(2026, 5, 9, 17);

const PERSON: Person = {
  id: "person-dana",
  display_name: "Dana Reyes",
  aliases: ["Dana R."],
  calendar_emails: ["dana@example.com"],
  organization: "Acme",
  summary: {
    text: "Dana runs pricing at Acme.",
    generated_at_utc_ms: JUNE,
    model_id: "apple-intelligence",
  },
  created_at_utc_ms: JANUARY,
  updated_at_utc_ms: JUNE,
};
const OTHER_PERSON: Person = {
  ...PERSON,
  id: "person-amir",
  display_name: "Amir Khan",
  aliases: [],
  calendar_emails: ["amir@example.com"],
  organization: null,
  summary: null,
};
const ENTRY: PersonListEntry = {
  person: PERSON,
  meetings_count: 2,
  last_meeting_at_utc_ms: JUNE,
  suggested_count: 1,
  evidence_sources: ["calendar", "speaker", "title"],
  confirmed_count: 2,
  last_meeting: {
    session_id: "meeting-current",
    title: "Current review",
    at_ms: JUNE,
    headline: { kind: "ledger", text: "Pricing is still open." },
  },
};
const OTHER_ENTRY: PersonListEntry = {
  person: OTHER_PERSON,
  meetings_count: 1,
  last_meeting_at_utc_ms: FEBRUARY,
  suggested_count: 0,
  evidence_sources: ["calendar"],
  confirmed_count: 1,
  last_meeting: {
    session_id: "meeting-planning",
    title: "Planning",
    at_ms: FEBRUARY,
    headline: { kind: "summary", text: "Launch planning." },
  },
};
const CONFIRMED_LINK: PersonMeetingLink = {
  meeting: {
    id: "meeting-planning",
    title: "Planning",
    at_utc_ms: JANUARY,
    headline: "The launch checklist still needs an owner.",
    series_number: 2,
  },
  source: "calendar",
  confidence: "confirmed",
};
const CURRENT_LINK: PersonMeetingLink = {
  meeting: {
    id: "meeting-current",
    title: "Current review",
    at_utc_ms: JUNE,
    headline: "Pricing is still open.",
    series_number: 3,
  },
  source: "speaker",
  confidence: "confirmed",
};
const SUGGESTED_LINK: PersonMeetingLink = {
  meeting: {
    id: "meeting-suggested",
    title: "Launch sync",
    at_utc_ms: FEBRUARY,
    headline: null,
    series_number: 1,
  },
  source: "title",
  confidence: "suggested",
};
const DETAIL: PersonDetail = {
  person: PERSON,
  links: [CONFIRMED_LINK, CURRENT_LINK, SUGGESTED_LINK],
  open_loops: [
    {
      loop_id: "meeting-january:loop:a1b2c3d4e5f60718",
      meeting_id: CONFIRMED_LINK.meeting.id,
      title: CONFIRMED_LINK.meeting.title,
      at_utc_ms: JANUARY,
      text: "Who owns the launch checklist?",
      owner_person_id: PERSON.id,
      status: "open",
      direction: "waiting_on",
      waiting_on_stale: true,
      carried_since_at_utc_ms: JANUARY,
      carried_into_meeting_id: null,
    },
  ],
  commitments: [
    {
      loop_id: "meeting-january:commitment:8796a5b4c3d2e1f0",
      meeting_id: CONFIRMED_LINK.meeting.id,
      title: CONFIRMED_LINK.meeting.title,
      at_utc_ms: JANUARY,
      text: "Dana will send the tier comparison.",
      status: "done",
      direction: "waiting_on",
      waiting_on_stale: false,
      resolved_at_utc_ms: JUNE,
    },
  ],
  talk_share_avg_permille: 347,
  documents: [],
};
const DOCUMENT: Document = {
  summary: {
    id: "document-1",
    title: "Account notes",
    source_name: "account-notes.md",
    media_type: "text/markdown",
    created_at_utc_ms: FEBRUARY,
  },
  content: "Dana prefers a concise weekly update.",
};

const list = (
  entries: React.ComponentProps<typeof PeopleListView>["entries"],
) =>
  render(
    <PeopleListView
      entries={entries}
      error={false}
      onOpenPerson={noop}
      onOpenOrganization={noop}
      onRetry={noop}
    />,
  );

const detail = (personDetail: PersonDetail, documents: Document[]) =>
  render(
    <PersonDetailView
      detail={personDetail}
      people={[ENTRY, OTHER_ENTRY]}
      documents={documents}
      documentsLoadFailed={false}
      pending={false}
      onBack={noop}
      onRename={noop}
      onMerge={noop}
      onDelete={noop}
      onUnlink={noop}
      onSplit={noop}
      onConfirmLink={noop}
      onImportDocument={noop}
      onDeleteDocument={noop}
      onOpenMeeting={noop}
      onOpenOrganization={noop}
      onRegenerateSummary={noop}
    />,
  );

describe("People list", () => {
  test("keeps the shared page measure and states an absence in one line", () => {
    const markup = list([]);

    expect(markup).toContain("max-w-[760px]");
    expect(occurrences(markup, 'data-slot="people-empty-row"')).toBe(1);
    // One sentence and no glyph over it: an absence is not an illustration.
    expect(occurrences(markup, "<svg")).toBe(0);
    expect(markup).not.toContain('data-slot="person-card"');
  });

  test("shows a quiet organization label beside the relationship facts", () => {
    const markup = list([ENTRY]);

    expect(markup).toContain('data-slot="person-card"');
    expect(markup).toContain("Dana Reyes");
    expect(markup).toContain("2 meetings");
    expect(markup).toContain('data-slot="person-organization"');
    expect(markup).toContain("Acme");
    expect(markup).toContain("Last met");
    /* The line is the whole row. The initial bubble, the last meeting's
     * headline, the evidence chips and the suggested-links footer all moved to
     * the person's own page, which is what the row opens — a list of people is
     * not the place to read one person's meeting. */
    expect(markup).not.toContain('data-slot="meeting-person"');
    expect(markup).not.toContain('data-slot="suggested-links"');
    expect(markup).not.toContain("Pricing is still open.");
    expect(markup).not.toContain("Calendar");
    expect(markup).not.toContain("Launch sync");
    expect(markup).not.toContain(">Dismiss</button>");
    expect(markup).not.toContain(">Confirm</button>");
  });

  /* The strip is derived from the rows already on screen, so it can only ever
   * name an organization somebody in the list carries — and a list where
   * nobody does draws no strip rather than an empty one. */
  test("names the organizations the loaded people carry, once each", () => {
    const markup = list([ENTRY, { ...ENTRY, person: { ...PERSON } }]);

    expect(markup).toContain('data-slot="organizations-strip"');
    expect(occurrences(markup, 'data-slot="organization-chip"')).toBe(1);
    expect(markup).toContain("Organizations");

    expect(list([OTHER_ENTRY])).not.toContain(
      'data-slot="organizations-strip"',
    );
  });
});

describe("organization detail", () => {
  const organization = () =>
    render(
      <OrganizationView
        detail={{
          name: "Acme",
          people: [ENTRY],
          recent_meetings: [
            {
              id: CONFIRMED_LINK.meeting.id,
              title: "Planning",
              at_utc_ms: JANUARY,
              headline: "The launch checklist still needs an owner.",
              series_number: 2,
            },
          ],
          open_loops: DETAIL.open_loops,
        }}
        onBack={noop}
        onOpenPerson={noop}
        onOpenMeeting={noop}
      />,
    );

  test("reads across its people: who is here, what you met about, what is open", () => {
    const markup = organization();

    expect(markup).toContain("max-w-[760px]");
    expect(markup).toContain("Acme");
    expect(markup).toContain("1 person");
    expect(occurrences(markup, 'data-slot="organization-person"')).toBe(1);
    expect(occurrences(markup, 'data-slot="organization-meeting"')).toBe(1);
    expect(occurrences(markup, 'data-slot="organization-loop"')).toBe(1);
    expect(markup).toContain("Who owns the launch checklist?");
    /* An organization is a slice of People, not a fourth noun: nothing here
     * renames, merges or deletes anything. */
    expect(markup).not.toContain('aria-label="Person actions"');
  });
});

describe("person detail", () => {
  test("an empty person is a name and a menu, not seven labelled absences", () => {
    const markup = detail(
      {
        ...DETAIL,
        person: { ...PERSON, aliases: [], calendar_emails: [], summary: null },
        links: [],
        open_loops: [],
        commitments: [],
        talk_share_avg_permille: null,
      },
      [],
    );

    expect(markup).toContain("max-w-[760px]");
    /* The name and the menu are the page; a section with nothing in it is not
     * on it at all, so no heading stands over a sentence saying so. */
    expect(markup).toContain("Dana Reyes");
    expect(markup).toContain('aria-label="More"');
    expect(occurrences(markup, 'data-slot="people-empty-row"')).toBe(0);
    for (const heading of [
      "About",
      "Meetings together",
      "Open loops",
      "Why Sona links this person",
      "Commitments",
      "Meeting cadence",
      "Documents",
    ])
      expect(markup).not.toContain(heading);
    expect(markup).not.toContain('data-slot="person-summary"');
    expect(markup).not.toContain('data-slot="person-cadence"');
  });

  test("reads from what is open, through the archive, down to the evidence", () => {
    const markup = detail(DETAIL, [DOCUMENT]);

    expect(markup.indexOf("Open loops")).toBeLessThan(
      markup.indexOf("Meetings together"),
    );
    expect(markup.indexOf("Meetings together")).toBeLessThan(
      markup.indexOf("Why Sona links this person"),
    );
    /* Three kinds of evidence across three links, counted into one sentence
     * rather than one row per kind - and under it what else this person is
     * called and the address an invite reaches them at, which used to crowd
     * the title. Nothing here is pressable, so nothing here is a chip. */
    expect(occurrences(markup, 'data-slot="person-evidence-sources"')).toBe(1);
    expect(markup).toContain(
      "Calendar 1 meeting · Speaker 1 meeting · Title 1 meeting",
    );
    expect(markup).toContain("Also: Dana R.");
    expect(markup).toContain("Invited as dana@example.com");
    expect(markup).not.toContain('data-slot="person-evidence-row"');
  });

  /* Three sentences under the name, with the engine that wrote them and when.
   * No paragraph, no card: the verb that asks for one is in the page's menu,
   * so a card standing empty to hold a lone button is a card the page does
   * not need. */
  test("shows the relationship paragraph with its engine, and no card without one", () => {
    const written = detail(DETAIL, []);

    expect(written).toContain('data-slot="person-summary"');
    expect(written).toContain("Dana runs pricing at Acme.");
    expect(written).toContain("Written by apple-intelligence");
    expect(written).not.toContain(">Regenerate</button>");

    const blank = detail(
      { ...DETAIL, person: { ...PERSON, summary: null } },
      [],
    );

    expect(blank).not.toContain('data-slot="person-summary"');
    expect(blank).not.toContain("No summary yet.");
    expect(blank).not.toContain("Written by");
  });

  test("renders cadence, relationship facts, links, and imported context", () => {
    const markup = detail(DETAIL, [DOCUMENT]);
    /* The organization is a link to its own page, and it shares the header's
     * one Meta line with when you last met. */
    expect(markup).toContain('data-slot="person-organization"');
    expect(markup).toContain(">Acme</button>");
    expect(markup).toContain("Last met");

    const cadenceBars =
      markup.match(
        /<svg[^>]*data-slot="person-cadence-bars"[\s\S]*?<\/svg>/u,
      )?.[0] ?? "";
    expect(cadenceBars).not.toBe("");
    expect(occurrences(cadenceBars, "<rect")).toBe(6);
    expect(markup).toContain("34.7%");
    expect(markup).toContain("Who owns the launch checklist?");
    expect(markup).toContain("Dana will send the tier comparison.");
    expect(occurrences(markup, 'data-slot="person-meeting"')).toBe(3);
    expect(markup).toContain('data-slot="person-document"');
    expect(markup).toContain("Dana prefers a concise weekly update.");
    /* Every verb about who this person is - rename, regenerate, import,
     * merge, split, delete - waits behind the one trigger on the title line,
     * so no named button for any of them stands on the page. Four triggers:
     * three meeting rows and the header. */
    expect(markup).not.toContain(">Rename</button>");
    expect(markup).not.toContain(">Split person</button>");
    expect(markup).not.toContain(">Import document</button>");
    expect(markup).not.toContain('title="Rename"');
    expect(markup).toContain('aria-label="More"');
    expect(occurrences(markup, 'data-slot="dropdown-menu-trigger"')).toBe(4);
    /* An open loop reaches the meeting it came from through the citation mark
     * at the end of its sentence, and the mark names that meeting for anyone
     * who cannot see the line under it. One per ledger row: the open loop and
     * the commitment. */
    expect(occurrences(markup, 'data-slot="ledger-jump"')).toBe(2);
    expect(occurrences(markup, 'aria-label="Open Planning,')).toBe(2);
  });

  /* D27: a person page answers two questions, so it shows two lists. Grouping
   * them under one heading each is the whole feature — "what did I promise
   * Dana" and "what is Dana sitting on" were previously one undifferentiated
   * column, and the page could not tell you which was which. */
  test("groups what the user owes apart from what this person owes", () => {
    const markup = detail(
      {
        ...DETAIL,
        open_loops: [
          { ...DETAIL.open_loops[0], direction: "waiting_on" },
          {
            ...DETAIL.open_loops[0],
            loop_id: "meeting-january:loop:00112233445566ff",
            text: "Confirm the rebate spreadsheet owner",
            direction: "mine",
            waiting_on_stale: false,
          },
        ],
      },
      [],
    );

    expect(markup).toContain("I owe");
    expect(markup).toContain("Waiting on Dana Reyes");
    // The user's own line comes first: it is the one they can act on.
    expect(markup.indexOf("Confirm the rebate spreadsheet owner")).toBeLessThan(
      markup.indexOf("Who owns the launch checklist?"),
    );
  });

  /* The stale mark only ever lands on a row somebody else owes. A backlog of
   * the user's own work is theirs to schedule; marking it overdue would be the
   * app nagging its user about a decision it did not make. */
  test("marks an overdue handoff and never the user's own backlog", () => {
    const overdue = detail(DETAIL, []);
    expect(occurrences(overdue, 'data-slot="loop-stale"')).toBe(1);
    expect(overdue).toContain("Overdue");

    const mine = detail(
      {
        ...DETAIL,
        open_loops: [
          {
            ...DETAIL.open_loops[0],
            direction: "mine",
            waiting_on_stale: false,
          },
        ],
        commitments: [],
      },
      [],
    );
    expect(mine).not.toContain('data-slot="loop-stale"');
  });

  /* D18: a person page reads the loop's live state, not a copy of the words.
   * A commitment the review screen already settled must not read the same as
   * one still owed - which is the only thing the status word is for. "Open"
   * under a heading that says "Open loops" is the heading again, and a date
   * the sentence above already cites is that date again. */
  test("names a settled loop, and dates only one that outlived its meeting", () => {
    const markup = detail(DETAIL, []);

    expect(occurrences(markup, 'data-slot="loop-status"')).toBe(1);
    expect(markup).toContain(">Done<");
    expect(markup).not.toContain(">Open<");
    expect(markup).not.toContain("Open since");

    const carried = detail(
      {
        ...DETAIL,
        open_loops: DETAIL.open_loops.map((loop) => ({
          ...loop,
          carried_since_at_utc_ms: loop.at_utc_ms - 60_000,
        })),
      },
      [],
    );
    expect(carried).toContain("Open since");
  });
});

describe("People projections", () => {
  test("buckets only confirmed links into six UTC months", () => {
    expect(
      monthlyMeetingCadence(
        [CONFIRMED_LINK, CURRENT_LINK, SUGGESTED_LINK],
        Date.UTC(2026, 5, 20),
      ),
    ).toEqual([1, 0, 0, 0, 0, 1]);
  });

  test("shows PREVIOUSLY TOGETHER only when the meeting projection has a prior meeting", () => {
    const context: MeetingPersonContextRow = {
      person_id: PERSON.id,
      display_name: PERSON.display_name,
      evidence_source: "speaker",
      meetings_together: 2,
      last_prior_meeting: {
        id: CONFIRMED_LINK.meeting.id,
        title: CONFIRMED_LINK.meeting.title,
        at_utc_ms: JANUARY,
        headline: CONFIRMED_LINK.meeting.headline,
      },
      top_open_loop: DETAIL.open_loops[0],
    };
    const rows = previouslyTogetherRows([context]);

    expect(rows).toEqual([
      {
        personId: PERSON.id,
        displayName: PERSON.display_name,
        meetingsCount: 1,
        lastMeetingAtUtcMs: JANUARY,
        openLoop: "Who owns the launch checklist?",
      },
    ]);
    const markup = render(
      <PreviouslyTogetherBandView rows={rows} onOpenPerson={noop} />,
    );
    expect(markup).toContain('data-slot="previously-together"');
    expect(markup).toContain('data-slot="meeting-person"');
    expect(
      render(<PreviouslyTogetherBandView rows={[]} onOpenPerson={noop} />),
    ).toBe("");
  });
});

describe("follow-up agent prompt", () => {
  test("sends only current ledger commitments and open loops", () => {
    const ledger: MeetingLedger = {
      headline: "Pricing remains open.",
      threads: [],
      open_loops: [
        {
          question: "Which tier does the trial convert into?",
          instead: "The meeting moved on without an answer.",
          at_ms: 12_000,
          citations: [],
        },
      ],
      commitments: [
        {
          who: "Dana",
          what: "Send the tier comparison",
          firmness: "firm",
          receipt: {
            quote: "I will send the tier comparison.",
            speaker: "Dana",
            t_ms: 9_000,
            citations: [],
          },
        },
      ],
      stances: [],
      caveats: [],
      receipts: { status: "verified" },
    };
    /* A regeneration leaves the revision it replaced in the list, ahead of
     * the one that superseded it. The prompt is sent to a person, so a
     * commitment the meeting no longer holds anyone to must not reach it. */
    const superseded: MeetingLedger = {
      ...ledger,
      open_loops: [],
      commitments: [
        {
          who: "Amir",
          what: "Discount the annual plan",
          firmness: "firm",
          receipt: {
            quote: "I will discount the annual plan.",
            speaker: "Amir",
            t_ms: 4_000,
            citations: [],
          },
        },
      ],
    };
    /* The builder reads only the ledger; the other artifact fields satisfy
     * the generated shape with empty values production also starts from. */
    const emptyText = { text: "", citations: [] };
    const content = (revision: MeetingLedger) => ({
      summary: emptyText,
      outline: [],
      decisions: [],
      action_items: [],
      key_questions: [],
      risks: [],
      follow_up_draft: emptyText,
      ledger: revision,
    });
    const snapshot: FollowUpAgentMessageSource = {
      session: { title: "Pricing review" },
      artifacts: [
        { state: "out_of_date", content: content(superseded) },
        { state: "current", content: content(ledger) },
      ],
    };
    const message = buildFollowUpAgentMessage(snapshot, i18n.t.bind(i18n));

    expect(message).toContain("Pricing review");
    expect(message).toContain("- Dana: Send the tier comparison");
    expect(message).toContain("- Which tier does the trial convert into?");
    expect(message).not.toContain("Amir");
  });
});

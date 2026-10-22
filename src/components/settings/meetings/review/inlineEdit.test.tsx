import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { MeetingReviewSnapshot } from "@/bindings";
import {
  committedEdit,
  inlineEditKeys,
  type InlineEditKeyEvent,
} from "./inlineEdit";
import { SpeakerNameEditor } from "./SpeakerRoster";
import {
  TranscriptTurn,
  type TranscriptTurnProps,
  type TranscriptTurnSegment,
} from "./TranscriptTab";

/* Editing on demand, from both sides.
 *
 * The reading surface hides every field and every destructive action until
 * somebody asks for one, so two things have to be nailed down: that the
 * resting turn really is prose, and that the corrections still reach the
 * store. This runner has no DOM, so a control is pressed the way a person
 * names it — by the label on it — and the element tree the component produced
 * is what carries that label to the press. */

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
const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localePath, "utf8")) },
  },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});

const render = (node: React.ReactElement) =>
  renderToStaticMarkup(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);

/** The element tree a component produced, rendered once so its hooks run. */
const treeOf = (
  component: () => React.ReactNode | Promise<React.ReactNode>,
): React.ReactNode => {
  let tree: React.ReactNode = null;
  const Harness = () => {
    // SAFETY: React 19 types every component as possibly async; the two
    // rendered here are plain synchronous functions.
    tree = component() as React.ReactNode;
    return null;
  };
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <Harness />
    </I18nextProvider>,
  );
  return tree;
};

interface ControlProps {
  onClick?: () => void;
  "aria-label"?: string;
  /** What a hovered control says it does, when its own words are the text. */
  title?: string;
  children?: React.ReactNode;
}

const words = (node: React.ReactNode): string => {
  if (node === null || node === undefined || node === true || node === false) {
    return "";
  }
  if (Array.isArray(node)) return node.map(words).join("");
  if (React.isValidElement(node)) {
    // SAFETY: this walker only meets elements this test file rendered, and
    // every one of them carries at most the ControlProps members it reads.
    return words((node.props as ControlProps).children);
  }
  return String(node);
};

const controls = (node: React.ReactNode): ControlProps[] => {
  if (Array.isArray(node)) return node.flatMap(controls);
  if (!React.isValidElement(node)) return [];
  // SAFETY: same first-party tree as `words`; only ControlProps members read.
  const props = node.props as ControlProps;
  return [
    ...(props.onClick === undefined ? [] : [props]),
    ...controls(props.children),
  ];
};

/** Press the control a person would name, by the words or the name on it. */
const press = (node: React.ReactNode, label: string) => {
  const control = controls(node).find(
    (candidate) =>
      candidate["aria-label"] === label ||
      candidate.title === label ||
      words(candidate.children) === label,
  );
  if (control?.onClick === undefined) {
    throw new Error(`no control labelled "${label}"`);
  }
  control.onClick();
};

const keyPress = (key: string, value: string, shiftKey = false) => {
  let prevented = false;
  const event: InlineEditKeyEvent = {
    key,
    shiftKey,
    currentTarget: { value },
    preventDefault: () => {
      prevented = true;
    },
  };
  return { event, prevented: () => prevented };
};

describe("what a finished inline edit asks for", () => {
  test("a changed draft is the trimmed words", () => {
    expect(
      committedEdit("  We ship on Thursday.  ", "We ship this week."),
    ).toBe("We ship on Thursday.");
  });

  test("an untouched draft asks for nothing, whitespace and all", () => {
    expect(
      committedEdit("We ship this week.", "We ship this week."),
    ).toBeNull();
    expect(
      committedEdit("  We ship this week. ", "We ship this week."),
    ).toBeNull();
  });

  /* Emptying the field is somebody changing their mind, not a request to
   * delete the words: removal is its own action with its own receipt. */
  test("an emptied draft asks for nothing", () => {
    expect(committedEdit("", "We ship this week.")).toBeNull();
    expect(committedEdit("   \n ", "We ship this week.")).toBeNull();
  });
});

describe("the keyboard half of commit-on-intent", () => {
  test("Enter commits what the field holds, and goes no further", () => {
    const asked: string[] = [];
    const typed = keyPress("Enter", "We ship on Thursday.");
    inlineEditKeys(
      (draft) => asked.push(draft),
      () => asked.push("cancelled"),
    )(typed.event);

    expect(asked).toEqual(["We ship on Thursday."]);
    expect(typed.prevented()).toBe(true);
  });

  test("Shift+Enter is a line break, so nothing is committed", () => {
    const asked: string[] = [];
    const typed = keyPress("Enter", "We ship on Thursday.", true);
    inlineEditKeys(
      (draft) => asked.push(draft),
      () => asked.push("cancelled"),
    )(typed.event);

    expect(asked).toEqual([]);
    expect(typed.prevented()).toBe(false);
  });

  test("Escape abandons the draft without asking for a write", () => {
    const asked: string[] = [];
    const typed = keyPress("Escape", "Something half-typed");
    inlineEditKeys(
      (draft) => asked.push(draft),
      () => asked.push("cancelled"),
    )(typed.event);

    expect(asked).toEqual(["cancelled"]);
    expect(typed.prevented()).toBe(true);
  });
});

/* One voice, two sentences: the shape the transcript is set in now. The
 * second one is the kind that used to get a bordered row and a repeated
 * speaker name all to itself. */
const SENTENCES: TranscriptTurnSegment[] = [
  {
    segmentId: "segment-1",
    time: "0:12",
    text: "We ship the meetings redesign this week.",
    removed: false,
    landed: false,
    flashing: false,
    editing: false,
  },
  {
    segmentId: "segment-2",
    time: "0:15",
    text: "Okay.",
    removed: false,
    landed: false,
    flashing: false,
    editing: false,
  },
];

const TURN: TranscriptTurnProps = {
  speaker: "Dana",
  time: "0:12",
  segments: SENTENCES,
  query: "",
  disabled: false,
  onOpenEdit: () => undefined,
  onCommit: () => undefined,
  onCancel: () => undefined,
  onRemove: () => undefined,
};

const turn = (overrides: Partial<TranscriptTurnProps> = {}) =>
  render(<TranscriptTurn {...TURN} {...overrides} />);

/** The same turn with one of its sentences open in its editor. */
const correcting = (
  segmentId: string,
  extra: Partial<TranscriptTurnSegment> = {},
): TranscriptTurnProps => ({
  ...TURN,
  segments: SENTENCES.map((segment) =>
    segment.segmentId === segmentId
      ? { ...segment, editing: true, ...extra }
      : segment,
  ),
});

describe("a transcript turn at rest", () => {
  const markup = turn();

  test("is one paragraph: the sentences run together under one name", () => {
    expect(markup).toContain("We ship the meetings redesign this week.");
    expect(markup).toContain("Okay.");
    /* The whole readability fix, in one number: the name is said once for the
     * stretch this voice held, not once per sentence. */
    expect(markup.split(">Dana<").length - 1).toBe(1);
    expect(markup).not.toContain("<textarea");
    expect(markup).not.toContain(">Remove this turn<");
  });
  test("every sentence keeps the dom id a citation resolves it by", () => {
    expect(markup).toContain('id="meeting-transcript-segment-segment-1"');
    expect(markup).toContain('id="meeting-transcript-segment-segment-2"');
    expect(markup.split('tabindex="0"').length - 1).toBe(2);
  });
  test("a transcript nobody may correct is text, with nothing to press", () => {
    const locked = turn({ disabled: true });
    expect(locked).toContain("We ship the meetings redesign this week.");
    expect(locked).not.toContain('title="Edit this turn"');
    expect(locked).not.toContain('tabindex="0"');
  });

  test("the live filter marks the words it matched inside the prose", () => {
    const filtered = turn({ query: "meetings" });
    expect(filtered).toContain("<mark");
    expect(filtered).toContain(">meetings</mark>");
    expect(filtered).toContain("We ship the ");
  });
});

describe("a transcript sentence being corrected", () => {
  const markup = render(<TranscriptTurn {...correcting("segment-1")} />);

  test("the field takes that sentence's place and no other", () => {
    expect(markup).toContain("<textarea");
    expect(markup).toContain('aria-label="Transcript turn"');
    expect(markup).toContain("We ship the meetings redesign this week.");
    /* The sentence being corrected is the field; the one beside it is still
     * the prose it was, id and all. */
    expect(markup).not.toContain('id="meeting-transcript-segment-segment-1"');
    expect(markup).toContain('id="meeting-transcript-segment-segment-2"');
    expect(markup).toContain("Okay.");
  });

  test("is the only place removal is offered, and it reads as removal", () => {
    expect(markup.split(">Remove this turn<").length - 1).toBe(1);
  });

  test("removal asks the store to remove that sentence, not to rewrite it", () => {
    const asked: [string, string][] = [];
    const tree = treeOf(() =>
      TranscriptTurn({
        ...correcting("segment-2"),
        onRemove: (segmentId, current) => asked.push([segmentId, current]),
      }),
    );

    press(tree, "Remove this turn");
    expect(asked).toEqual([["segment-2", "Okay."]]);
  });

  test("pressing a sentence opens that sentence's own field", () => {
    const opened: string[] = [];
    const tree = treeOf(() =>
      TranscriptTurn({
        ...TURN,
        onOpenEdit: (segmentId) => opened.push(segmentId),
      }),
    );

    press(tree, "Edit this turn");
    expect(opened).toEqual(["segment-1"]);
  });

  /* A sentence already struck out has nothing left to remove, so the action
   * that would do it a second time is not drawn. */
  test("a sentence already removed is offered no second removal", () => {
    const gone = render(
      <TranscriptTurn {...correcting("segment-1", { removed: true })} />,
    );
    expect(gone).toContain("<textarea");
    expect(gone).not.toContain(">Remove this turn<");
  });
});

const speaker = (
  speakerId: string,
  displayName: string,
): MeetingReviewSnapshot["speakers"][number] => ({
  speaker_id: speakerId,
  session_id: "meeting-1",
  source_kind: "microphone",
  display_name: displayName,
  revision: 1,
});

const DANA = speaker("speaker-1", "Dana");
const AMIR = speaker("speaker-2", "Amir");

describe("a speaker chip being renamed", () => {
  const markup = render(
    <SpeakerNameEditor
      speaker={DANA}
      others={[AMIR]}
      onCommit={() => undefined}
      onCancel={() => undefined}
      onMerge={() => undefined}
      onCorrect={() => undefined}
    />,
  );

  test("is a field holding the name, with no Save beside it", () => {
    expect(markup).toContain('aria-label="Speaker name"');
    expect(markup).toContain('value="Dana"');
  });
  test("merging asks the store to fold this speaker into the named one", () => {
    const asked: string[] = [];
    const tree = treeOf(() =>
      SpeakerNameEditor({
        speaker: DANA,
        others: [AMIR],
        onCommit: () => undefined,
        onCancel: () => undefined,
        onMerge: (targetSpeakerId) => asked.push(targetSpeakerId),
        onCorrect: () => undefined,
      }),
    );

    press(tree, "Same person as Amir");
    expect(asked).toEqual(["speaker-2"]);
  });
  test("the only speaker in the room is nobody else, so nothing to merge", () => {
    const alone = render(
      <SpeakerNameEditor
        speaker={DANA}
        others={[]}
        onCommit={() => undefined}
        onCancel={() => undefined}
        onMerge={() => undefined}
        onCorrect={() => undefined}
      />,
    );

    expect(alone).toContain('aria-label="Speaker name"');
    expect(alone).not.toContain("Same person as");
  });
});

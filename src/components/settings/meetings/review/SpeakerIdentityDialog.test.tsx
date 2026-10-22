import React from "react";
import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import en from "@/i18n/locales/en/translation.json";
import type { PersonListEntry } from "@/bindings";
import { Dialog } from "@/components/vg/dialog";
import { SpeakerIdentityDialogForm } from "./SpeakerIdentityDialog";

/* The dialog body, not the dialog. Radix renders modal content through a
 * portal, which this DOM-less runner drops, so the body is the surface a test
 * can read; the dialog around it is two props and a remount key.
 *
 * Copy is read out of the catalog rather than retyped here, because what these
 * tests pin is that the speaker's name reaches the sentence and that the
 * destructive control asks first — not the wording, which the locale pass
 * owns. Each assertion checks the interpolated name is in the string it
 * expects, so a missing key cannot make the comparison pass by matching
 * itself. */

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: en } },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});

const people: PersonListEntry[] = [
  {
    person: {
      id: "person-1",
      display_name: "Jordan",
      aliases: [],
      calendar_emails: [],
      organization: null,
      summary: null,
      created_at_utc_ms: 1,
      updated_at_utc_ms: 1,
    },
    meetings_count: 0,
    last_meeting_at_utc_ms: null,
    suggested_count: 0,
    evidence_sources: [],
    confirmed_count: 0,
    last_meeting: null,
  },
];

type FormProps = React.ComponentProps<typeof SpeakerIdentityDialogForm>;

const noop = () => undefined;

const formProps = (overrides: Partial<FormProps>): FormProps => ({
  mode: "label",
  speakerName: "Speaker 1",
  people,
  peopleLoading: false,
  peopleLoadFailed: false,
  pending: false,
  unknownConfirming: false,
  onRetryPeople: noop,
  onSave: noop,
  onUnknownRequest: noop,
  onUnknownCancel: noop,
  onSkip: noop,
  onNotNow: noop,
  ...overrides,
});

const render = (overrides: Partial<FormProps>) =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <Dialog open>
        <SpeakerIdentityDialogForm {...formProps(overrides)} />
      </Dialog>
    </I18nextProvider>,
  );

/** The element tree the body produced, rendered once so its hooks run. */
const treeOf = (props: FormProps): React.ReactNode => {
  let tree: React.ReactNode = null;
  const Harness = () => {
    // SAFETY: React 19 types every component as possibly async; this one is a
    // plain synchronous function.
    tree = SpeakerIdentityDialogForm(props) as React.ReactNode;
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

/** Press the control a person would name, by the words on it. */
const press = (node: React.ReactNode, label: string) => {
  const control = controls(node).find(
    (candidate) => words(candidate.children) === label,
  );
  if (control?.onClick === undefined) {
    throw new Error(`no control labelled "${label}"`);
  }
  control.onClick();
};

const markUnknown = i18n.t("meetings.review.markUnknown");

describe("the speaker identity dialog", () => {
  /* The dialog reopens on the next unresolved speaker without closing, so
   * "3 speakers to label" is the same question three times. The name is the
   * only thing that tells them apart. */
  test("the label question names the speaker it is asking about", () => {
    const title = i18n.t("meetings.review.labelSpeakerNamed", {
      speaker: "Speaker 1",
    });

    expect(title).toContain("Speaker 1");
    expect(render({})).toContain(title);
  });

  test("a correction names the speaker and reads as a correction", () => {
    const correcting = i18n.t("meetings.review.correctSpeakerNamed", {
      speaker: "Dana",
    });
    const labelling = i18n.t("meetings.review.labelSpeakerNamed", {
      speaker: "Dana",
    });
    const markup = render({ mode: "correct", speakerName: "Dana" });

    expect(correcting).toContain("Dana");
    expect(markup).toContain(correcting);
    expect(markup).not.toContain(labelling);
  });

  test("offers the deliberate label choices with remembering unchecked", () => {
    const markup = render({});

    expect(markup).toContain(i18n.t("meetings.review.rememberVoice"));
    expect(markup).toContain(i18n.t("meetings.review.notNow"));
    expect(markup).toContain(markUnknown);
    expect(markup).toContain(">Save<");
    expect(markup).toContain('aria-checked="false"');
  });

  /* Marking a speaker unknown writes "Unknown" over whatever name the
   * transcript carried and deletes the voice samples saved from that speaker,
   * and neither comes back. One press must not be able to do that. */
  test("marking a speaker unknown asks before it discards anything", () => {
    const done: string[] = [];
    const tree = treeOf(
      formProps({
        onUnknownRequest: () => done.push("asked"),
        onSkip: () => done.push("marked"),
      }),
    );

    press(tree, markUnknown);

    expect(done).toEqual(["asked"]);
  });

  test("the question names what marking unknown deletes", () => {
    const sentence = i18n.t("meetings.review.markUnknownDescription", {
      speaker: "Dana",
    });
    const markup = render({ speakerName: "Dana", unknownConfirming: true });

    expect(sentence).toContain("Dana");
    expect(markup).toContain(sentence);
    expect(markup).not.toContain(">Save<");
  });

  test("answering the question marks the speaker unknown", () => {
    const done: string[] = [];
    const tree = treeOf(
      formProps({
        unknownConfirming: true,
        onUnknownRequest: () => done.push("asked"),
        onSkip: () => done.push("marked"),
      }),
    );

    press(tree, markUnknown);

    expect(done).toEqual(["marked"]);
  });

});

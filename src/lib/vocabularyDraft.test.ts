import { describe, expect, test } from "bun:test";
import {
  duplicateSpokenPhrases,
  mergeAppliedCsv,
  resolveRefreshDraft,
  samePairEntries,
  spokenMatchKey,
} from "./vocabularyDraft";

const entry = (spoken: string, written: string) => ({ spoken, written });

describe("resolveRefreshDraft", () => {
  test("adopts the saved list when the user has no unsaved edits", () => {
    const saved = [entry("open ai", "OpenAI")];
    const incoming = [entry("open ai", "OpenAI"), entry("sona", "Sona")];
    expect(resolveRefreshDraft(saved, saved, incoming)).toEqual(incoming);
  });

  test("keeps the draft when an unrelated refresh arrives mid-edit", () => {
    const previousSaved = [entry("open ai", "OpenAI")];
    const draft = [entry("open ai", "OpenAI"), entry("new term", "NewTerm")];
    // Same persisted value as before: the user typed a row but never saved.
    const incoming = [entry("open ai", "OpenAI")];
    expect(resolveRefreshDraft(draft, previousSaved, incoming)).toEqual(draft);
  });

  test("keeps the draft even when the refresh brings a new saved list", () => {
    const previousSaved = [entry("open ai", "OpenAI")];
    const draft = [entry("open ai", "OpenAI"), entry("new term", "NewTerm")];
    const incoming = [entry("open ai", "OpenAI"), entry("other", "Other")];
    expect(resolveRefreshDraft(draft, previousSaved, incoming)).toEqual(draft);
  });

});

describe("mergeAppliedCsv", () => {
  test("keeps unsaved local drafts that the CSV does not define", () => {
    const drafts = [entry("draft term", "DraftTerm")];
    const applied = [entry("open ai", "OpenAI")];
    expect(mergeAppliedCsv(drafts, applied)).toEqual([
      entry("open ai", "OpenAI"),
      entry("draft term", "DraftTerm"),
    ]);
  });

  test("does not duplicate a draft the CSV also defines", () => {
    const drafts = [entry("open ai", "OpenAI")];
    const applied = [entry("open ai", "OpenAI")];
    expect(mergeAppliedCsv(drafts, applied)).toEqual([
      entry("open ai", "OpenAI"),
    ]);
  });

  test("returns the applied list unchanged when there are no drafts", () => {
    const applied = [entry("open ai", "OpenAI")];
    expect(mergeAppliedCsv([], applied)).toEqual(applied);
  });
});

describe("samePairEntries", () => {
  test("compares pairs by both fields", () => {
    expect(samePairEntries([entry("a", "A")], [entry("a", "A")])).toBe(true);
    expect(samePairEntries([entry("a", "A")], [entry("a", "B")])).toBe(false);
    expect(samePairEntries([], [])).toBe(true);
  });
});

describe("spokenMatchKey", () => {
  test("keeps letters and numbers, drops everything else", () => {
    expect(spokenMatchKey("Open AI")).toBe("openai");
    expect(spokenMatchKey("open-ai")).toBe("openai");
    expect(spokenMatchKey("GPT 4o")).toBe("gpt4o");
  });

  test("is empty when a phrase has no letter or number", () => {
    // The backend refuses these outright, so the editor has to spot them.
    expect(spokenMatchKey("...")).toBe("");
    expect(spokenMatchKey(" ")).toBe("");
  });

  test("normalizes non-latin phrases without stripping them", () => {
    expect(spokenMatchKey("Ünïcode!")).toBe("ünïcode");
    expect(spokenMatchKey("你好, world")).toBe("你好world");
  });
});

describe("duplicateSpokenPhrases", () => {
  test("reports a phrase two rows claim after normalization", () => {
    expect(
      duplicateSpokenPhrases([
        entry("Open AI", "OpenAI"),
        entry("open-ai", "Open AI Inc"),
        entry("sona", "Sona"),
      ]),
    ).toEqual(["Open AI"]);
  });

  test("names each conflicting phrase once", () => {
    expect(
      duplicateSpokenPhrases([
        entry("a", "A"),
        entry("a", "B"),
        entry("a", "C"),
      ]),
    ).toEqual(["a"]);
  });

  test("ignores rows with no usable spoken phrase", () => {
    expect(
      duplicateSpokenPhrases([entry("...", "A"), entry("!!", "B")]),
    ).toEqual([]);
  });

  test("finds nothing in a list of distinct phrases", () => {
    expect(
      duplicateSpokenPhrases([entry("one", "1"), entry("two", "2")]),
    ).toEqual([]);
  });
});

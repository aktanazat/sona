import { describe, expect, test } from "bun:test";

import {
  median,
  normalizeWords,
  percentile,
  summarizeSet,
  wordEdits,
  type ClipResult,
} from "./dictation-eval";

describe("normalizeWords", () => {
  test("drops punctuation and symbols, lowercases and folds compatibility forms", () => {
    expect(normalizeWords("Thanks, Kenji. I'll pay $427.50 — ﬁne?")).toEqual([
      "thanks",
      "kenji",
      "i",
      "ll",
      "pay",
      "427",
      "50",
      "fine",
    ]);
  });

  test("is empty for silence", () => {
    expect(normalizeWords("  ... ")).toEqual([]);
  });
});

describe("wordEdits", () => {
  test("counts nothing for an exact match", () => {
    expect(wordEdits(["a", "b", "c"], ["a", "b", "c"])).toBe(0);
  });

  test("counts substitutions, insertions and deletions", () => {
    expect(wordEdits(["a", "b", "c"], ["a", "x", "c"])).toBe(1);
    expect(wordEdits(["a", "b"], ["a", "b", "c", "d"])).toBe(2);
    expect(wordEdits(["a", "b", "c"], ["c"])).toBe(2);
    expect(wordEdits(["a"], [])).toBe(1);
    expect(wordEdits([], ["a", "b"])).toBe(2);
  });

  test("scores spoken numbers against digits as errors", () => {
    expect(
      wordEdits(normalizeWords("twenty seven"), normalizeWords("27")),
    ).toBe(2);
  });
});

describe("median and percentile", () => {
  test("median averages the middle pair of an even count", () => {
    expect(median([3, 1, 2])).toBe(2);
    expect(median([4, 1, 3, 2])).toBe(2.5);
  });

  test("percentile takes the nearest rank", () => {
    const values = Array.from({ length: 20 }, (_, index) => index + 1);
    expect(percentile(values, 95)).toBe(19);
    expect(percentile([7], 95)).toBe(7);
    expect(
      percentile(
        Array.from({ length: 30 }, (_, index) => 30 - index),
        95,
      ),
    ).toBe(29);
  });
});

function result(
  reference: string,
  written: string,
  hypothesis: string,
  decodeMs: number[],
  audioSeconds: number,
): ClipResult {
  return {
    clip: { set: "say", id: reference, wav: "", reference, written },
    audioSeconds,
    loadMs: 500,
    decodeMs,
    backend: "cpu",
    hypothesis,
  };
}

describe("summarizeSet", () => {
  test("weights WER by reference words and RTF by audio time", () => {
    const summary = summarizeSet([
      result(
        "pay twenty seven dollars",
        "pay $27",
        "Pay $27.",
        [120, 100, 110],
        2,
      ),
      result("hello there", "hello there", "Hello there.", [300, 200, 250], 3),
    ]);
    expect(summary.clips).toBe(2);
    expect(summary.reference_words).toBe(6);
    expect(summary.word_edits).toBe(3);
    expect(summary.wer_percent).toBe(50);
    expect(summary.written_wer_percent).toBe(0);
    expect(summary.latency_ms).toEqual({ median: 180, p95: 250 });
    expect(summary.rtf).toBe(0.072);
  });
});

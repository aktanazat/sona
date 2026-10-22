import { describe, expect, test } from "bun:test";
import {
  actionItemKey,
  formatPatience,
  formatTalkDuration,
  formatTalkShare,
} from "./meetingAnalytics";

/* The command wrappers are one invoke call each and are exercised by the app;
 * what is worth pinning here is the arithmetic a reader sees on the strip. */

describe("formatTalkShare", () => {
  test("reads per-mille as a whole percent", () => {
    expect(formatTalkShare(1_000)).toBe("100%");
    expect(formatTalkShare(431)).toBe("43%");
    expect(formatTalkShare(0)).toBe("0%");
  });

  test("rounds to nearest rather than truncating", () => {
    expect(formatTalkShare(435)).toBe("44%");
    expect(formatTalkShare(434)).toBe("43%");
  });
});

describe("formatTalkDuration", () => {
  test("stays in seconds below a minute", () => {
    expect(formatTalkDuration(0)).toBe("0s");
    expect(formatTalkDuration(41_000_000_000)).toBe("41s");
  });

  test("switches to m:ss at a minute", () => {
    expect(formatTalkDuration(60_000_000_000)).toBe("1:00");
    expect(formatTalkDuration(125_000_000_000)).toBe("2:05");
  });

  test("never reports negative time", () => {
    expect(formatTalkDuration(-5_000_000_000)).toBe("0s");
  });
});

describe("formatPatience", () => {
  test("marks an absent median rather than showing zero", () => {
    expect(formatPatience(null)).toBe("—");
  });

  test("keeps sub-second gaps in milliseconds", () => {
    expect(formatPatience(0)).toBe("0ms");
    expect(formatPatience(640)).toBe("640ms");
  });

  test("shows one decimal once a gap passes a second", () => {
    expect(formatPatience(1_000)).toBe("1.0s");
    expect(formatPatience(2_450)).toBe("2.5s");
  });
});

describe("actionItemKey", () => {
  test("separates the same index across generated revisions", () => {
    expect(
      actionItemKey("artifact-a", 0) === actionItemKey("artifact-b", 0),
    ).toBe(false);
    expect(
      actionItemKey("artifact-a", 0) === actionItemKey("artifact-a", 1),
    ).toBe(false);
  });
});

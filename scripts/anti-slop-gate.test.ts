import { describe, expect, test } from "bun:test";
import {
  compareToBaseline,
  parseBaseline,
  summarize,
  type BaselineEntry,
} from "./anti-slop-gate";

function entry(file: string, rule: string, count: number): BaselineEntry {
  return { file, rule, severity: "error", count };
}

describe("summarize", () => {
  test("counts diagnostics per file and rule in a stable order", () => {
    expect(
      summarize([
        {
          filename: "src/b.ts",
          code: "anti-slop(no-runtime-typeof)",
          severity: "error",
        },
        {
          filename: "src/a.ts",
          code: "anti-slop(no-runtime-typeof)",
          severity: "error",
        },
        {
          filename: "src/a.ts",
          code: "anti-slop(no-runtime-typeof)",
          severity: "error",
        },
        {
          filename: "src/a.ts",
          code: "eslint(no-unused-vars)",
          severity: "warning",
        },
      ]),
    ).toEqual([
      entry("src/a.ts", "anti-slop(no-runtime-typeof)", 2),
      {
        file: "src/a.ts",
        rule: "eslint(no-unused-vars)",
        severity: "warning",
        count: 1,
      },
      entry("src/b.ts", "anti-slop(no-runtime-typeof)", 1),
    ]);
  });

  test("normalizes diagnostic paths to forward slashes", () => {
    expect(
      summarize([
        {
          filename: "scripts\\prepare-agent-hook.ts",
          code: "anti-slop(no-runtime-typeof)",
          severity: "error",
        },
      ]),
    ).toEqual([
      entry("scripts/prepare-agent-hook.ts", "anti-slop(no-runtime-typeof)", 1),
    ]);
  });
});

describe("compareToBaseline", () => {
  test("passes when the manifest matches, whatever order it is stored in", () => {
    const baseline = [
      entry("src/b.ts", "anti-slop(no-runtime-typeof)", 1),
      entry("src/a.ts", "anti-slop(no-object-parameters)", 3),
    ];
    const current = [
      entry("src/a.ts", "anti-slop(no-object-parameters)", 3),
      entry("src/b.ts", "anti-slop(no-runtime-typeof)", 1),
    ];

    expect(compareToBaseline(baseline, current)).toEqual({
      added: [],
      resolved: [],
    });
  });

  test("reports a rule that has not fired in that file before", () => {
    const drift = compareToBaseline(
      [entry("src/a.ts", "anti-slop(no-object-parameters)", 1)],
      [
        entry("src/a.ts", "anti-slop(no-object-parameters)", 1),
        entry("src/a.ts", "anti-slop(no-runtime-typeof)", 1),
      ],
    );

    expect(drift.added).toEqual([
      {
        file: "src/a.ts",
        rule: "anti-slop(no-runtime-typeof)",
        baselineCount: 0,
        currentCount: 1,
      },
    ]);
    expect(drift.resolved).toEqual([]);
  });

  test("reports one more finding of a rule the file already violates", () => {
    const drift = compareToBaseline(
      [
        entry(
          "src/a.ts",
          "anti-slop(require-safety-comment-for-type-assertion)",
          4,
        ),
      ],
      [
        entry(
          "src/a.ts",
          "anti-slop(require-safety-comment-for-type-assertion)",
          5,
        ),
      ],
    );

    expect(drift.added).toEqual([
      {
        file: "src/a.ts",
        rule: "anti-slop(require-safety-comment-for-type-assertion)",
        baselineCount: 4,
        currentCount: 5,
      },
    ]);
    expect(drift.resolved).toEqual([]);
  });

  test("reports a fixed finding as resolved, never as new", () => {
    const drift = compareToBaseline(
      [
        entry("src/a.ts", "anti-slop(no-runtime-typeof)", 2),
        entry("src/b.ts", "anti-slop(no-object-parameters)", 1),
      ],
      [entry("src/a.ts", "anti-slop(no-runtime-typeof)", 1)],
    );

    expect(drift.added).toEqual([]);
    expect(drift.resolved).toEqual([
      {
        file: "src/a.ts",
        rule: "anti-slop(no-runtime-typeof)",
        baselineCount: 2,
        currentCount: 1,
      },
      {
        file: "src/b.ts",
        rule: "anti-slop(no-object-parameters)",
        baselineCount: 1,
        currentCount: 0,
      },
    ]);
  });
});

describe("parseBaseline", () => {
  test("accepts the manifest the gate writes", () => {
    const baseline = parseBaseline(
      JSON.stringify({
        oxlintVersion: "1.79.0",
        entries: [entry("src/a.ts", "anti-slop(no-runtime-typeof)", 2)],
      }),
    );

    expect(baseline.oxlintVersion).toBe("1.79.0");
    expect(baseline.entries).toHaveLength(1);
  });

  test("refuses a hand-edited count that would hide findings", () => {
    expect(() =>
      parseBaseline(
        JSON.stringify({
          oxlintVersion: "1.79.0",
          entries: [entry("src/a.ts", "anti-slop(no-runtime-typeof)", 0)],
        }),
      ),
    ).toThrow("positive integer count");
  });

  test("refuses a manifest with no recorded oxlint version", () => {
    expect(() => parseBaseline(JSON.stringify({ entries: [] }))).toThrow(
      "needs an oxlintVersion string",
    );
  });
});

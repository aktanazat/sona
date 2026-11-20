const noFixedWait =
  "Tests must not wait on wall-clock time. Use fake timers (vi.useFakeTimers / bun's jest.useFakeTimers) or await the actual event.";

export default {
  ignorePatterns: [
    ".agent/**",
    ".agents/**",
    ".claude/**",
    ".codex/**",
    ".continue/**",
    ".cursor/**",
    ".gemini/**",
    ".opencode/**",
    ".pi/**",
    ".roo/**",
    ".windsurf/**",
    "tools/oxlint/anti-slop/**",
  ],
  jsPlugins: [
    {
      name: "anti-slop",
      specifier: "./tools/oxlint/anti-slop/index.ts",
    },
    {
      name: "landmines-testing",
      specifier: "@interfere/landmines/testing",
    },
  ],
  rules: {
    "anti-slop/no-chained-type-assertions": "error",
    "anti-slop/no-conditional-empty-object-spread": "error",
    "anti-slop/no-known-value-widening": "error",
    "anti-slop/no-module-mocking": "error",
    "anti-slop/no-object-parameters": "error",
    "anti-slop/no-reflect-apply": "error",
    "anti-slop/no-reflect-get": "error",
    "anti-slop/no-runtime-typeof": "error",
    "anti-slop/no-shape-in-symbol-names": "error",
    "anti-slop/no-unknown-parameters": "error",
    "anti-slop/no-unknown-returns": "error",
    "anti-slop/no-unknown-type-aliases": "error",
    "anti-slop/no-unsafe-dictionary-type": "error",
    "anti-slop/no-widen-then-assert": "error",
    "anti-slop/require-safety-comment-for-type-assertion": "error",
    "landmines-testing/no-fixed-sleep": "error",
    "landmines-testing/no-flake-masking": "error",
    "landmines-testing/no-test-doubles": "error",
    "landmines-testing/no-unsignaled-poll": "error",
    "landmines-testing/strict-msw": "error",
  },
  overrides: [
    {
      files: ["**/*.test.*", "**/*.spec.*", "**/test/**", "**/tests/**"],
      rules: {
        "no-restricted-globals": [
          "error",
          { name: "setTimeout", message: noFixedWait },
        ],
        "no-restricted-properties": [
          "error",
          {
            object: "globalThis",
            property: "setTimeout",
            message: noFixedWait,
          },
          { object: "window", property: "setTimeout", message: noFixedWait },
          { object: "self", property: "setTimeout", message: noFixedWait },
          { object: "Bun", property: "sleep", message: noFixedWait },
          { object: "Bun", property: "sleepSync", message: noFixedWait },
        ],
        "no-restricted-imports": [
          "error",
          {
            paths: [
              "timers",
              "node:timers",
              "timers/promises",
              "node:timers/promises",
            ].map((name) => ({ name, message: noFixedWait })),
          },
        ],
      },
    },
  ],
};

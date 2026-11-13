import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "jsdom",
    // Node 25+ defines its own localStorage, which hides jsdom's.
    execArgv: ["--no-experimental-webstorage"],
    include: ["test/**/*.viewer.spec.ts"],
    coverage: {
      provider: "istanbul",
      include: ["public/viewer.js"],
      reporter: ["text", "json-summary"],
      reportsDirectory: "coverage/viewer",
    },
  },
});

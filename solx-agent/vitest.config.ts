import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // The harness tests need no DOM, but tests/mount.test.ts imports the
    // built bundle the way a browser would -- see docs/widget-system.md on
    // why a jsdom harness substitutes for browser automation here.
    environment: "jsdom",
  },
});

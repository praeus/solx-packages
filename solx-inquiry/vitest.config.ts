import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // The pipeline tests need no DOM, but tests/mount.test.ts imports the
    // built bundle the way a browser would -- the same jsdom substitution for
    // browser automation docs/widget-system.md explains for solx-agent.
    environment: "jsdom",
  },
});

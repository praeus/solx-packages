import { resolve } from "node:path";
import react from "@vitejs/plugin-react";
import cssInjectedByJsPlugin from "vite-plugin-css-injected-by-js";
import { defineConfig, type UserConfig } from "vite";

export interface WidgetConfigOptions {
  /** Path (relative to the widget package's cwd) to the build entry, e.g. "src/main.ts". */
  entry: string;
  /** Output file name inside dist/ -- this is the bin_name a widget's install.solx uploads. */
  outFile: string;
}

/**
 * Vite config for a widget bundle: library mode, single ES file, no
 * code-splitting, CSS injected via a <style> tag at runtime instead of
 * emitted as a separate asset. All three follow from the same constraint --
 * a widget bundle is fetched as bytes and `import()`ed from a blob URL
 * (solx-core/docs/widget-actions.md §3), which has no base path to resolve
 * a sibling chunk or stylesheet against, so everything has to live in the
 * one file.
 */
export function createWidgetConfig({ entry, outFile }: WidgetConfigOptions): UserConfig {
  return defineConfig({
    plugins: [react(), cssInjectedByJsPlugin()],
    // React/ReactDOM's own source is full of `process.env.NODE_ENV` checks.
    // A normal Vite app build replaces these automatically, but a blob-URL
    // bundle never runs through Vite's dev server or an app's index.html --
    // it's `import()`ed standalone in a page that has no `process` global at
    // all, so an unreplaced reference throws "process is not defined" at
    // mount time. Bundling react/react-dom in (rather than treating them as
    // externals) means their source ends up subject to the same constraint
    // as the widget's own code.
    define: {
      "process.env.NODE_ENV": JSON.stringify("production"),
    },
    build: {
      outDir: "dist",
      lib: {
        entry: resolve(entry),
        formats: ["es"],
        fileName: () => outFile,
      },
      rollupOptions: {
        output: {
          inlineDynamicImports: true,
        },
      },
    },
  });
}

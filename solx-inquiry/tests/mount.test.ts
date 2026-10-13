/**
 * Mounts the *built* bundle the way solx-web does, against a scripted stub
 * client.
 *
 * This is the check that catches what a typecheck cannot: the bundle is
 * `import()`ed standalone into a page with no `process` global, so an
 * unreplaced `process.env.NODE_ENV` throws only at mount time. It also proves
 * the client injection is wired end to end.
 *
 * Requires `npm run build` first; skips itself if `dist/` is absent rather
 * than failing.
 */

import { existsSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { beforeAll, describe, expect, test } from "vitest";

const BUNDLE = resolve(__dirname, "../dist/solx-inquiry.js");
const built = existsSync(BUNDLE);

interface WidgetElement extends HTMLElement {
  fields?: unknown;
  solxClient?: unknown;
}

describe.skipIf(!built)("the built bundle", () => {
  let el: WidgetElement;
  const calls: string[] = [];

  beforeAll(async () => {
    await import(pathToFileURL(BUNDLE).href);

    el = document.createElement("solx-inquiry-widget") as WidgetElement;
    el.fields = { title: "Inquiry" };
    el.solxClient = {
      actions: {
        exec: async (path: string, name: string) => {
          calls.push(path + "/" + name);
          if (name === "ollama-list-models") {
            return { success: true, result: { models: [{ name: "qwen3:4b", capabilities: ["tools"] }] } };
          }
          return { success: true, result: { items: [] } };
        },
      },
    };
    document.body.appendChild(el);
    // React renders concurrently and jsdom is slow to get there, so this
    // needs real headroom -- 50ms is not enough.
    await new Promise((r) => setTimeout(r, 400));
  });

  test("registers its custom element and renders into a shadow root", () => {
    expect(customElements.get("solx-inquiry-widget")).toBeTruthy();
    expect(el.shadowRoot).toBeTruthy();
    expect(el.shadowRoot!.innerHTML).not.toContain("needs a host that supplies one");
  });

  test("reaches its injected client", () => {
    expect(calls).toContain("/packages/solx-ollama/ollama-list-models");
    expect(calls).toContain("/builtin/document/search-documents");
  });

  test("offers the composer with no inquiry open", () => {
    expect(el.shadowRoot!.innerHTML).toContain("What do you want to know?");
  });

  test("shows the options toggle, collapsed, so tuning stays out of the way", () => {
    const html = el.shadowRoot!.innerHTML;
    expect(html).toContain("Options");
    expect(html).not.toContain("Max lookups per question");
  });

  test("has no approval UI -- every verb is a read", () => {
    expect(el.shadowRoot!.innerHTML).not.toContain("Approval needed");
  });
});

/**
 * Mounts the *built* bundle the way solx-web does, against a scripted stub
 * client.
 *
 * This is the check that catches what a typecheck cannot: the bundle is
 * `import()`ed standalone into a page with no `process` global, so an
 * unreplaced `process.env.NODE_ENV` throws only at mount time and the build
 * itself succeeds in silence. It also proves the client injection is wired
 * end to end -- the stub's `exec` has to be reachable from inside the
 * rendered component.
 *
 * Requires `npm run build` first; skips itself if `dist/` is absent rather
 * than failing, so `vitest run` is useful before a build.
 */

import { existsSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { beforeAll, describe, expect, test } from "vitest";

const BUNDLE = resolve(__dirname, "../dist/solx-agent.js");
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

    el = document.createElement("solx-agent-widget") as WidgetElement;
    el.fields = { title: "Agent" };
    el.solxClient = {
      actions: {
        exec: async (path: string, name: string) => {
          calls.push(path + "/" + name);
          if (name === "ollama-list-models") {
            return {
              success: true,
              result: { models: [{ name: "qwen3:4b", capabilities: ["tools"] }] },
            };
          }
          return { success: true, result: { hits: [] } };
        },
      },
    };
    document.body.appendChild(el);
    // Let the mount effects settle. React renders concurrently and jsdom is
    // slow to get there, so this needs real headroom -- 50ms was not enough.
    await new Promise((r) => setTimeout(r, 400));
  });

  test("registers its custom element and renders into a shadow root", () => {
    expect(customElements.get("solx-agent-widget")).toBeTruthy();
    expect(el.shadowRoot).toBeTruthy();
    expect(el.shadowRoot!.innerHTML).not.toContain("needs a host that supplies one");
  });

  test("renders the setup surface, because a session cannot start without it", () => {
    const html = el.shadowRoot!.innerHTML;
    expect(html).toContain("Tools and setup");
    expect(html).toContain("allowed");
  });

  test("reaches its injected client", () => {
    // Models and history are both fetched through the client on mount.
    expect(calls).toContain("/packages/solx-ollama/ollama-list-models");
    expect(calls).toContain("/builtin/document/search_documents");
  });

  test("offers the composer, with no session open", () => {
    const html = el.shadowRoot!.innerHTML;
    expect(html).toContain("What should the agent do?");
  });
});

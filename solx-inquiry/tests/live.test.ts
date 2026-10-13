/**
 * The one test that talks to a real solx-server and a real Ollama, skipped
 * unless `SOLX_TOKEN` is set.
 *
 * `tests/fakeHost.ts` mirrors solx-core's behaviour as it was read out of the
 * source; if solx-core changes, every other test keeps passing and the package
 * breaks. This is what covers that gap, and it is worth its cost for exactly
 * the reason solx-agent's does: the `q: null` class of bug is invisible to a
 * fake that was written from the same misunderstanding.
 *
 *   solx-server --port 8791 &
 *   SOLX_TOKEN=<token> SOLX_MODEL=qwen3:4b npx vitest run tests/live.test.ts
 *
 * Run it against a *current* solx-server build. A stale binary will disagree
 * with the source about wire keys and send you chasing bugs that are not there.
 */

import { describe, expect, test } from "vitest";
import { buildCorpus, executePlan, hostFromClient, normalizeOptions } from "../src/harness/index";
import type { Host } from "../src/harness/index";

const TOKEN = process.env.SOLX_TOKEN || "";
const URL = process.env.SOLX_SERVER_URL || "http://127.0.0.1:8791";
const live = TOKEN.trim().length > 0;

function liveHost(): Host {
  return hostFromClient({
    actions: {
      async exec(path: string, name: string, params?: unknown) {
        const ref = (path === "/" ? "" : path) + "/" + name;
        const res = await fetch(URL + "/actions" + ref, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: "Bearer " + TOKEN },
          body: JSON.stringify(params ?? {}),
        });
        if (!res.ok) return { success: false, message: await res.text(), result: null };
        return (await res.json()) as { success?: boolean; message?: string | null; result?: unknown };
      },
    },
  });
}

describe.skipIf(!live)("against a live solx-server", () => {
  test("the corpus index reads without erroring on a no-query list", async () => {
    // The `q: null` trap: `entity-list-document-paths` takes a ListOptions with
    // no `q`, and this package must never put one on the wire for it.
    const corpus = await buildCorpus(liveHost(), false);
    expect(typeof corpus.text).toBe("string");
  });

  test("a no-query list of documents dispatches successfully", async () => {
    const result = await executePlan(liveHost(), [{ verb: "list", limit: 5 }]);
    expect(result.steps[0].outcome).not.toBe("error");
  });

  test("a search dispatches and returns whole documents", async () => {
    const result = await executePlan(liveHost(), [{ verb: "search", q: "the", limit: 3 }]);
    const step = result.steps[0];
    expect(step.outcome).not.toBe("error");
    // Whole documents, contents included -- a hit with no contents would mean
    // the server predates commit 330197d.
    for (const doc of result.documents) {
      expect(doc.ref).toContain("/");
    }
  });

  test("types lists without touching a search index it does not have", async () => {
    const result = await executePlan(liveHost(), [{ verb: "types", limit: 5 }]);
    expect(result.steps[0].outcome).not.toBe("error");
  });

  test("a bogus query returns empty, not an error", async () => {
    const result = await executePlan(liveHost(), [
      { verb: "search", q: "zzzznonexistenttermzzzz" },
    ]);
    expect(result.steps[0].outcome).toBe("empty");
  });
});

describe("options", () => {
  test("clamps to the hard caps regardless of input", () => {
    const o = normalizeOptions({ max_docs: 9999, max_plan_steps: 9999, doc_budget: 999999 });
    expect(o.max_docs).toBeLessThanOrEqual(30);
    expect(o.max_plan_steps).toBeLessThanOrEqual(8);
    expect(o.doc_budget).toBeLessThanOrEqual(60000);
  });
});

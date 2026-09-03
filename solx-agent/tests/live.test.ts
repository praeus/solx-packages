/**
 * A live run against a real solx-server and a real model.
 *
 * **Skipped unless `SOLX_TOKEN` is set**, because it needs a running server,
 * an Ollama holding a tool-capable model, and it spends real model calls.
 *
 * It is the check the fake host cannot make. `tests/fakeHost.ts` mirrors
 * solx-core's behaviour *as it was read out of the source* -- the camelCase
 * query keys, `typeRef` required on create, search hits carrying no contents,
 * the `not found` error text. If any of those changes, every other test here
 * keeps passing and the package breaks. This one would notice.
 *
 * ```sh
 * solx-server --port 8791 &
 * SOLX_TOKEN=$(...) SOLX_MODEL=qwen3:4b npx vitest run tests/live.test.ts
 * ```
 */

import { describe, expect, test } from "vitest";
import { addTurn, createSession } from "../src/harness/agent";
import { hostFromClient } from "../src/harness/host";
import { driveSession } from "../src/harness/loop";
import { summarize } from "../src/harness/turn";

const BASE = process.env.SOLX_BASE ?? "http://127.0.0.1:8791";
const TOKEN = process.env.SOLX_TOKEN;
const MODEL = process.env.SOLX_MODEL ?? "qwen3:4b";

const client = {
  actions: {
    async exec(path: string, name: string, params?: unknown) {
      const ref = (path === "/" ? "" : path) + "/" + name;
      const res = await fetch(BASE + "/actions" + ref, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: "Bearer " + TOKEN },
        body: JSON.stringify(params ?? {}),
      });
      if (!res.ok) return { success: false, message: await res.text(), result: null };
      return (await res.json()) as { success: boolean; message: string | null; result: unknown };
    },
  },
};

describe.skipIf(!TOKEN)("live", () => {
  test(
    "a real model drives a real turn, and the catalogue follows the conversation",
    async () => {
      const host = hostFromClient(client);
      const s = await createSession(host, "search documents", {
        model: MODEL,
        grant: [
          { path: "/builtin/document", actions: ["search_documents", "entity_get_document"] },
        ],
        max_iterations: 4,
      });
      // eslint-disable-next-line no-console
      console.log("session", s.id, "tools:", Object.keys(s.tools));
      expect(Object.keys(s.tools).length).toBeGreaterThan(0);

      const out = await driveSession(host, s, summarize(s, "running"), {
        // eslint-disable-next-line no-console
        onProgress: (r) => console.log("  ->", r.status, "turn iteration", r.turn_iteration),
      });
      // eslint-disable-next-line no-console
      console.log("status:", out.status, "|", (out.answer ?? "").slice(0, 200));

      // Any quiescent status is a pass: what is under test is that a real
      // model, real catalogue and real dispatch got through a turn, not that
      // a 4B model answered well.
      expect(["idle", "exhausted", "blocked"]).toContain(out.status);
      expect(s.messages.some((m) => m.role === "assistant")).toBe(true);

      // The second turn re-resolves the catalogue against its own message.
      await addTurn(host, s, "list files", { max_iterations: 2 });
      expect(s.turn_iteration, "the per-turn budget resets").toBe(0);
      expect(s.status).toBe("running");
    },
    900000,
  );
});

/**
 * Dispatch tests: the call shape and result plumbing, exercised without a
 * React tree or a browser. multi_inquire's own intent phase is what decides
 * direct vs. research (see solx-inquiry's intent.rs); this module has no
 * routing decision of its own left to test.
 */
import { describe, expect, it } from "vitest";
import { hostFromClient } from "../../solx-widgets/src/wrap/host";
import { dispatch, isRunnableActionHit } from "../src/dispatch";
import type { InquireHit } from "../src/types";
import { createFakeClient } from "./fakeClient";

const MULTI_INQUIRE_REF = "/packages/solx-inquiry/multi_inquire";

describe("dispatch", () => {
  it("starts a tracked multi_inquire call with the instruction, model, and session", async () => {
    const { client, respondTo } = createFakeClient();
    const host = hostFromClient(client);
    respondTo(MULTI_INQUIRE_REF, {
      result: {
        instruction: "hi",
        model: "llama3.2:1b",
        session: "/xprompt/sessions/s1",
        intent: { mode: "direct", inquiries: [] },
        responses: [{ text: "hello", memory: false, tags: [], citations: [], inquiry: null }],
        memories: [],
        scripts: [],
        next_prompt: null,
        hits: [],
        notes: [],
        errors: [],
      },
    });

    const handle = await dispatch(client, host, "session-1", "hi", "llama3.2:1b", "/xprompt/sessions/s1");
    expect(handle.invocationId).toBeTruthy();

    const result = await handle.result;
    expect(result.intent.mode).toBe("direct");
    expect(result.responses[0].text).toBe("hello");
  });

  it("tracks the call in the session's call log so it can be read back by the Console tab", async () => {
    const { client, respondTo } = createFakeClient();
    const host = hostFromClient(client);
    respondTo(MULTI_INQUIRE_REF, { result: { responses: [] } });

    await dispatch(client, host, "session-2", "hi", "m", "/xprompt/sessions/s2");

    const r = await host.call<{ contents?: { calls?: Array<{ actionRef: string; name: string }> } }>(
      "/builtin/document/entity-get-document",
      { path: "/xprompt/call-logs", name: "session-2" },
    );
    expect(r.contents?.calls).toHaveLength(1);
    expect(r.contents?.calls?.[0]).toMatchObject({ actionRef: MULTI_INQUIRE_REF, name: "multi_inquire" });
  });

  it("surfaces an inquire-mode result with hits and scripts intact", async () => {
    const { client, respondTo } = createFakeClient();
    const host = hostFromClient(client);
    respondTo(MULTI_INQUIRE_REF, {
      result: {
        intent: { mode: "inquire", inquiries: [{ kind: "actions", question: "how do I search?", terms: ["search"] }] },
        responses: [{ text: "Use search-documents.", memory: false, tags: [], citations: ["/notes/x"], inquiry: 0 }],
        memories: [],
        scripts: [{ title: "search", actions: ["/builtin/document/search-documents"], destructive: [], notes: [], steps: [] }],
        next_prompt: null,
        hits: [
          { source: "action", path: "/packages/solx-xprompt", name: "xprompt-widget", details: { category: "widgets" }, inquiry: 0 },
        ],
        notes: [],
        errors: [],
      },
    });

    const handle = await dispatch(client, host, "session-3", "how do I search?", "m", "/xprompt/sessions/s3");
    const result = await handle.result;
    expect(result.intent.mode).toBe("inquire");
    expect(result.hits).toHaveLength(1);
    expect(result.scripts).toHaveLength(1);
  });

  it("rejects when the invocation fails", async () => {
    const { client, respondTo } = createFakeClient();
    const host = hostFromClient(client);
    respondTo(MULTI_INQUIRE_REF, { error: "bad_params: multi_inquire requires a model" });

    const handle = await dispatch(client, host, "session-4", "hi", "m", "/xprompt/sessions/s4");
    await expect(handle.result).rejects.toThrow("bad_params");
  });
});

describe("isRunnableActionHit", () => {
  it("accepts action hits with a category", () => {
    const hit: InquireHit = {
      source: "action",
      path: "/p",
      name: "n",
      details: { category: "widgets" },
    };
    expect(isRunnableActionHit(hit)).toBe(true);
  });

  it("rejects document hits and uncategorized action hits", () => {
    expect(
      isRunnableActionHit({
        source: "document",
        path: "/p",
        name: "n",
        details: null,
      }),
    ).toBe(false);
    expect(
      isRunnableActionHit({
        source: "action",
        path: "/p",
        name: "n",
        details: null,
      }),
    ).toBe(false);
  });
});

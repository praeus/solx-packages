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

const MULTI_INQUIRE_REF = "/packages/solx-inquiry/multi-inquire";

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
    expect(r.contents?.calls?.[0]).toMatchObject({ actionRef: MULTI_INQUIRE_REF, name: "multi-inquire" });
  });

  it("forwards force_kind to multi_inquire when the caller asks for it", async () => {
    const { client, respondTo, startCalls } = createFakeClient();
    const host = hostFromClient(client);
    respondTo(MULTI_INQUIRE_REF, { result: { responses: [] } });

    await dispatch(
      client,
      host,
      "session-force",
      "do the wikipedia thing",
      "m",
      "/xprompt/sessions/sf",
      { forceKind: "actions" },
    );

    // `startCalls` records what the widget handed `invocations.start`,
    // which is the same `compact()`ed object that `multi_inquire` parses.
    const last = startCalls[startCalls.length - 1];
    expect(last.actionRef).toBe(MULTI_INQUIRE_REF);
    expect((last.params as { force_kind?: unknown }).force_kind).toBe("actions");
  });

  it("omits force_kind entirely when set to auto", async () => {
    const { client, respondTo, startCalls } = createFakeClient();
    const host = hostFromClient(client);
    respondTo(MULTI_INQUIRE_REF, { result: { responses: [] } });

    await dispatch(
      client,
      host,
      "session-auto",
      "hi",
      "m",
      "/xprompt/sessions/sa",
      { forceKind: undefined },
    );

    const last = startCalls[startCalls.length - 1];
    expect("force_kind" in ((last.params ?? {}) as Record<string, unknown>)).toBe(false);
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

describe("waiting for a turn's result", () => {
  const start = async (outcome: { result?: unknown; error?: string; status?: string }) => {
    const { client, respondTo } = createFakeClient();
    const host = hostFromClient(client);
    respondTo(MULTI_INQUIRE_REF, outcome);
    const handle = await dispatch(client, host, "session-1", "hi", "m", "/xprompt/sessions/s1");
    return handle.result;
  };

  it("rejects a terminal status that is not ok, even with no error message", async () => {
    // The store writes `error` as "" when there was none and the read-back
    // turns "" into absent, so cancelled/timeout/interrupted can arrive with
    // nothing to report. A guard that only checked `error` let those through
    // and returned `result` — null — as if it were an answer.
    for (const status of ["cancelled", "timeout", "interrupted"]) {
      await expect(start({ status, result: null })).rejects.toThrow(status);
    }
  });

  it("prefers the reported error when there is one", async () => {
    await expect(start({ error: "model exploded" })).rejects.toThrow("model exploded");
  });

  it("rejects an ok turn that returned nothing usable", async () => {
    // Rendering this as an empty answer would read as "the model had nothing
    // to say", which is a different and untrue statement.
    await expect(start({ result: null })).rejects.toThrow("no usable result");
    await expect(start({ result: "not an object" })).rejects.toThrow("no usable result");
  });

  it("normalises a partial result rather than handing it on raw", async () => {
    // A turn from an older build is missing whole fields; the components that
    // render it dereference them without guards.
    const result = await start({ result: { instruction: "x" } });
    expect(result.intent.mode).toBe("direct");
    expect(result.responses).toEqual([]);
    expect(result.hits).toEqual([]);
    expect(result.scripts).toEqual([]);
  });
});

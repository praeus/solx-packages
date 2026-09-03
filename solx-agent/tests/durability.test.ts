/**
 * Session naming, and the persistence that replaces what a single wasm
 * invocation used to give for free.
 *
 * The harness used to run inside one action call, so a turn was atomic: it
 * loaded, chatted, dispatched and saved, and nothing could interrupt it
 * halfway. Driving from a browser gives that up, so the loop persists after
 * the model replies, after every dispatched call, and again at flush. These
 * tests are about that being true, and about a half-finished turn being
 * resumable rather than lost.
 */

import { describe, expect, test, vi } from "vitest";
import { createSession } from "../src/harness/agent";
import { step } from "../src/harness/turn";
import { loadSession, newSessionId, callId } from "../src/harness/session";
import { DOCS_GRANT, fake, withDocActions } from "./fakeHost";
import type { Host } from "../src/harness/host";
import type { Session } from "../src/harness/types";

function seeded() {
  const { fake: f, host } = fake();
  withDocActions(f).action("/builtin/document/set_field_at_path", { description: "document field" });
  return { f, host };
}

async function session(host: Host) {
  return createSession(host, "document", { model: "m", grant: DOCS_GRANT });
}

describe("session names", () => {
  test("are words, not digits", async () => {
    const { host } = seeded();
    const id = await newSessionId(host);
    expect(id).toMatch(/^[a-z]+-[a-z]+$/);
  });

  test("a collision does not overwrite the session already holding it", async () => {
    const { f, host } = seeded();
    // entity_save_document is an upsert keyed on (path, name), so a collision
    // would not fail -- it would silently write over a running transcript.
    const taken = await newSessionId(host);
    f.doc("/agent/sessions/" + taken, { contents: { id: taken, status: "running" } });

    const seen = new Set<string>();
    for (let i = 0; i < 40; i++) seen.add(await newSessionId(host));
    expect(seen.has(taken), "a taken name must never be handed out again").toBe(false);
  });

  test("a read that broke is treated as taken, not as free", async () => {
    const { f, host } = seeded();
    const realExec = f.exec.bind(f);
    let broke = 0;
    // Anything other than a definite not-found must count as taken: the next
    // thing that happens is a write.
    vi.spyOn(f, "exec").mockImplementation((ref, p) => {
      if (ref === "/builtin/document/entity_get_document" && broke < 4) {
        broke++;
        return { success: false, message: "database is locked", result: null };
      }
      return realExec(ref, p);
    });

    const id = await newSessionId(host);
    expect(broke, "the broken reads were all treated as collisions").toBe(4);
    // Having exhausted the plain word pairs, it widened rather than reusing one.
    expect(id).toMatch(/-[0-9a-f]{4}$/);
    vi.restoreAllMocks();
  });
});

describe("call ids", () => {
  test("cover the arguments, not just the tool name", () => {
    // Approving a bare tool name would let a later iteration reuse the
    // approval for different arguments -- the same delete, a different doc.
    expect(callId("rm", { path: "/a" })).not.toBe(callId("rm", { path: "/b" }));
    expect(callId("rm", { path: "/a" })).toBe(callId("rm", { path: "/a" }));
  });
});

describe("persistence within a turn", () => {
  test("the model reply and its pending calls are stored before any dispatch", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyCalls(["act__builtin__document__set_field_at_path", { a: 1 }]);

    // Order matters: the session must be written between the chat call and
    // the first dispatch, or a tab closed mid-dispatch loses the record of
    // what was in flight.
    await step(host, s);
    const order = f.callNames();
    const chat = order.indexOf("/packages/solx-ollama/ollama-chat");
    const dispatch = order.indexOf("/builtin/document/set_field_at_path");
    const saveBetween = order.findIndex(
      (r, i) => i > chat && i < dispatch && r === "/builtin/document/entity_save_document",
    );
    expect(saveBetween, "a save lands between the chat and the dispatch").toBeGreaterThan(-1);
  });

  test("each dispatched call is persisted as it lands", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyCalls(
      ["act__builtin__document__set_field_at_path", { a: 1 }],
      ["act__builtin__document__set_field_at_path", { a: 2 }],
    );
    await step(host, s);

    const order = f.callNames();
    const first = order.indexOf("/builtin/document/set_field_at_path");
    const second = order.indexOf("/builtin/document/set_field_at_path", first + 1);
    const between = order
      .slice(first + 1, second)
      .filter((r) => r === "/builtin/document/entity_save_document");
    expect(between.length, "the first result is stored before the second call runs").toBe(1);
  });

  test("a turn abandoned mid-dispatch resumes instead of re-running what already ran", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyCalls(
      ["act__builtin__document__set_field_at_path", { a: 1 }],
      ["act__builtin__document__set_field_at_path", { a: 2 }],
    );

    // A closed tab does not throw -- the page simply stops. So rather than
    // injecting an error, capture what was actually written to the document
    // as the turn ran, and resume from the state a stopped page would have
    // left behind.
    const snapshots: Session[] = [];
    const realExec = f.exec.bind(f);
    vi.spyOn(f, "exec").mockImplementation((ref, p) => {
      const out = realExec(ref, p);
      if (ref === "/builtin/document/entity_save_document" && p.path === "/agent/sessions") {
        snapshots.push(JSON.parse(JSON.stringify(p.contents)) as Session);
      }
      return out;
    });
    await step(host, s);
    vi.restoreAllMocks();

    // The snapshot taken between the two dispatches: one result recorded,
    // one still outstanding. That this exists at all is the guarantee.
    const half = snapshots.find(
      (x) => x.pending.length === 2 && x.pending[0].result && !x.pending[1].result,
    );
    expect(half, "the document held a half-finished turn at some point").toBeTruthy();

    const before = f.refsCalled("/builtin/document/set_field_at_path").length;
    const out = await step(host, half!);

    // Finishing it dispatches only what was still outstanding.
    expect(f.refsCalled("/builtin/document/set_field_at_path").length).toBe(before + 1);
    expect(out.status).toBe("running");
    // And the flush still produced exactly one tool turn per call, which is
    // the invariant the transcript renderer depends on.
    expect(half!.messages.filter((m) => m.role === "tool").length).toBe(2);
    expect(half!.calls.length).toBe(2);
  });

  test("a reload sees the same thread, because the document is the record", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyCalls(["act__builtin__document__search_documents", { q: "x" }]);
    await step(host, s);
    f.replyText("here it is");
    await step(host, s);

    const reloaded = await loadSession(host, s.id);
    expect(reloaded.status).toBe("idle");
    expect(reloaded.answer).toBe("here it is");
    expect(reloaded.messages.length).toBe(s.messages.length);
    expect(reloaded.calls.length).toBe(1);
  });
});

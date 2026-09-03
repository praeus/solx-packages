/** One iteration: dispatch, refusal, the failure streak, approval, resume. */

import { describe, expect, test } from "vitest";
import { createSession } from "../src/harness/agent";
import { step } from "../src/harness/turn";
import { DOCS_GRANT, fake, withDocActions, type FakeHost } from "./fakeHost";
import type { Host } from "../src/harness/host";
import type { Session } from "../src/harness/types";

const SESSION_TYPE = "/packages/solx-agent/AgentSession";

function seeded() {
  const { fake: f, host } = fake();
  withDocActions(f).action("/builtin/document/set_field_at_path", {
    description: "set one document field",
  });
  return { f, host };
}

async function session(host: Host, opts: Record<string, unknown> = {}): Promise<Session> {
  return createSession(host, "document", { model: "m", grant: DOCS_GRANT, ...opts });
}

/** A host that also has one command action, reachable only by exact name. */
function withShell(): { f: FakeHost; host: Host; grant: { path: string; actions?: string[] }[] } {
  const { f, host } = seeded();
  f.action("/builtin/shell/run", { actionType: "command", description: "document shell" });
  return {
    f,
    host,
    grant: [{ path: "/builtin/document" }, { path: "/builtin/shell", actions: ["run"] }],
  };
}

describe("creating a session", () => {
  test("writes a session document with a typeRef", async () => {
    // Regression: entity_save_document requires typeRef on create and ignores
    // an unknown type_ref, so the snake_case spelling failed outright.
    const { f, host } = seeded();
    const s = await session(host);
    const doc = f.docs.get("/agent/sessions/" + s.id);
    expect(doc).toBeTruthy();
    expect(doc!.typeRef).toBe(SESSION_TYPE);
    expect(doc!.contents.status).toBe("running");
    expect(Object.keys(s.tools).length).toBeGreaterThan(0);
  });

  test("a first message matching nothing still gets the grant's tools", async () => {
    // The wording of one message must not decide whether a session can exist.
    const { host } = seeded();
    const s = await createSession(host, "xyzzy nothing matches this", {
      model: "m",
      grant: DOCS_GRANT,
    });
    expect(Object.values(s.tools)).toContain("/builtin/document/search_documents");
  });

  test("refuses a grant that reaches nothing at all", async () => {
    const { host } = seeded();
    await expect(
      createSession(host, "document", { model: "m", grant: [{ path: "/nowhere" }] }),
    ).rejects.toThrow(/no tools at all/);
  });

  test("is titled by its first message, so a session list says what it was about", async () => {
    const { f, host } = seeded();
    const s = await createSession(host, "search documents", {
      model: "m",
      grant: DOCS_GRANT,
    });
    const doc = f.docs.get("/agent/sessions/" + s.id);
    expect(doc!.title).toBe("search documents");
  });
});

describe("the loop", () => {
  test("a tool call is dispatched and answered with exactly one tool turn", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyCalls(["act__builtin__document__search_documents", { q: "x" }]);

    const out = await step(host, s);
    expect(out.status).toBe("running");
    expect(s.messages.filter((m) => m.role === "tool").length).toBe(1);
    expect(s.calls[0].outcome).toBe("ok");
    expect(s.calls[0].ref).toBe("/builtin/document/search_documents");
  });

  test("a name absent from the session map names nothing", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyCalls(["act__builtin__secrets__get_secret", {}]);

    await step(host, s);
    expect(s.calls[0].outcome).toBe("refused");
    expect(s.messages.at(-1)!.content).toMatch(/unknown tool/);
  });

  test("no tool calls hands the turn back as idle, not as an ending", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyText("here is the answer");
    const out = await step(host, s);
    expect(out.status).toBe("idle");
    expect(out.answer).toBe("here is the answer");
  });

  test("three iterations where every call fails end the session blocked", async () => {
    // Deliberately not search_documents: the fake answers that one itself, so
    // a failure injected on the row would never be reached.
    const { f, host } = seeded();
    f.actions_.get("/builtin/document/set_field_at_path")!.failWith = "boom";
    const s = await session(host);
    for (let i = 0; i < 3; i++) {
      f.replyCalls(["act__builtin__document__set_field_at_path", { a: 1 }]);
    }
    let out;
    for (let i = 0; i < 3; i++) out = await step(host, s);
    expect(out!.status).toBe("blocked");
  });

  test("the iteration budget is per turn, and the counter stays monotonic", async () => {
    const { f, host } = seeded();
    const s = await session(host, { max_iterations: 1 });
    f.replyCalls(["act__builtin__document__search_documents", { q: "x" }]);
    await step(host, s);
    const out = await step(host, s);
    expect(out.status).toBe("exhausted");
    expect(s.iteration).toBe(1);
    expect(s.turn_iteration).toBe(1);
  });
});

describe("approval", () => {
  test("a destructive call suspends, and an approval by call_id releases exactly it", async () => {
    const { f, host, grant } = withShell();
    const s = await createSession(host, "document", { model: "m", grant });

    f.replyCalls(["act__builtin__shell__run", { cmd: "rm -rf /" }]);
    const stopped = await step(host, s);
    expect(stopped.status).toBe("awaiting_approval");
    expect(stopped.pending.length).toBe(1);
    expect(stopped.pending[0].ref).toBe("/builtin/shell/run");
    expect(stopped.pending[0].arguments).toEqual({ cmd: "rm -rf /" });
    expect(f.refsCalled("/builtin/shell/run").length, "nothing ran before approval").toBe(0);

    const done = await step(host, s, [stopped.pending[0].call_id]);
    expect(done.status).toBe("running");
    expect(f.refsCalled("/builtin/shell/run").length).toBe(1);
  });

  test("an approval on the final allowed iteration still executes", async () => {
    const { f, host, grant } = withShell();
    const s = await createSession(host, "document", { model: "m", grant, max_iterations: 1 });

    f.replyCalls(["act__builtin__shell__run", { cmd: "rm -rf /" }]);
    const stopped = await step(host, s);
    expect(stopped.status).toBe("awaiting_approval");
    expect(stopped.turn_iteration, "the iteration was already spent asking").toBe(1);

    const done = await step(host, s, [stopped.pending[0].call_id]);
    expect(done.status, "the approved call must not be silently dropped").not.toBe("exhausted");
    expect(f.refsCalled("/builtin/shell/run").length).toBe(1);
  });

  test("resuming without naming a call denies it", async () => {
    const { f, host, grant } = withShell();
    const s = await createSession(host, "document", { model: "m", grant });

    f.replyCalls(["act__builtin__shell__run", { cmd: "x" }]);
    await step(host, s);
    await step(host, s, []);

    expect(s.calls[0].outcome).toBe("denied");
    expect(f.refsCalled("/builtin/shell/run").length).toBe(0);
  });

  test("an approval never carries into a later iteration", async () => {
    const { f, host, grant } = withShell();
    const s = await createSession(host, "document", { model: "m", grant });

    f.replyCalls(["act__builtin__shell__run", { cmd: "once" }]);
    const stopped = await step(host, s);
    await step(host, s, [stopped.pending[0].call_id]);
    expect(s.pending).toEqual([]);

    // The same tool with the same arguments in a later iteration is a fresh
    // decision -- the call_id was cleared, not banked.
    f.replyCalls(["act__builtin__shell__run", { cmd: "once" }]);
    const again = await step(host, s);
    expect(again.status).toBe("awaiting_approval");
    expect(f.refsCalled("/builtin/shell/run").length).toBe(1);
  });
});

describe("the gate re-runs at dispatch", () => {
  test("a tampered dispatch table buys nothing", async () => {
    const { f, host } = seeded();
    f.action("/builtin/secrets/get_secret", { description: "document secrets" });
    const s = await session(host);

    // Point a listed name at something the grant never permitted.
    s.tools["act__builtin__document__search_documents"] = "/builtin/secrets/get_secret";
    f.replyCalls(["act__builtin__document__search_documents", {}]);

    await step(host, s);
    expect(s.calls[0].outcome).toBe("refused");
    expect(f.refsCalled("/builtin/secrets/get_secret").length).toBe(0);
  });

  test("the model cannot rewrite its own session through a document action", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyCalls([
      "act__builtin__document__entity_save_document",
      { path: "/agent/sessions", name: s.id, contents: { status: "idle" } },
    ]);

    await step(host, s);
    expect(s.calls[0].outcome).toBe("refused");
    expect(s.messages.at(-1)!.content).toMatch(/agent-owned/);
  });

  test("an action that cannot be read back is refused, not waved through", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    // Listed in the map, but gone from the registry by the time it is called.
    f.actions_.delete("/builtin/document/search_documents");
    f.replyCalls(["act__builtin__document__search_documents", {}]);

    await step(host, s);
    expect(s.calls[0].outcome).toBe("refused");
    expect(s.messages.at(-1)!.content).toMatch(/no longer available/);
  });
});

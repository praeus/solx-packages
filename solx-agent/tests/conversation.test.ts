/**
 * A session as a conversation: adding turns, re-resolving the catalogue, and
 * the one thing an operator may change that a model may not.
 */

import { describe, expect, test } from "vitest";
import { addTurn, createSession, previewTools, widenGrant } from "../src/harness/agent";
import { step } from "../src/harness/turn";
import { loadSession } from "../src/harness/session";
import { DOCS_GRANT, fake, withDocActions } from "./fakeHost";
import type { Host } from "../src/harness/host";

function seeded() {
  const { fake: f, host } = fake();
  withDocActions(f).action("/builtin/document/set_field_at_path", {
    description: "set one document field",
  });
  return { f, host };
}

/** A second package the first message could never have selected tools from. */
function withMail(f: ReturnType<typeof seeded>["f"]) {
  return f.action("/packages/solx-google/send-gmail-message", {
    description: "send an email message",
  });
}

async function session(host: Host, message = "document") {
  return createSession(host, message, { model: "m", grant: DOCS_GRANT });
}

describe("adding a turn", () => {
  test("appends a user turn and makes a settled session runnable again", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyText("done");
    await step(host, s);
    expect(s.status).toBe("idle");

    const out = await addTurn(host, s, "and now the other one");
    expect(out.status).toBe("running");
    expect(s.answer).toBeNull();
    expect(s.messages.at(-1)).toEqual({ role: "user", content: "and now the other one" });
  });

  test("is refused while a decision is outstanding", async () => {
    const { f, host } = seeded();
    f.action("/builtin/shell/run", { actionType: "command", description: "document shell" });
    const s = await createSession(host, "document", {
      model: "m",
      grant: [{ path: "/builtin/document" }, { path: "/builtin/shell", actions: ["run"] }],
    });
    f.replyCalls(["act__builtin__shell__run", { cmd: "x" }]);
    await step(host, s);
    expect(s.status).toBe("awaiting_approval");

    // Accepting an unrelated instruction here would resolve those calls by
    // silently denying them.
    await expect(addTurn(host, s, "never mind")).rejects.toThrow(/awaiting approval/);
  });

  test("clears a failure streak, so a blocked session can be redirected", async () => {
    const { f, host } = seeded();
    f.actions_.get("/builtin/document/set_field_at_path")!.failWith = "boom";
    const s = await session(host);
    for (let i = 0; i < 3; i++) {
      f.replyCalls(["act__builtin__document__set_field_at_path", { a: 1 }]);
    }
    for (let i = 0; i < 3; i++) await step(host, s);
    expect(s.status).toBe("blocked");

    await addTurn(host, s, "try it another way");
    expect(s.status).toBe("running");
    expect(s.consecutive_failures).toBe(0);
  });

  test("gives an exhausted session a fresh allowance for the new turn", async () => {
    const { f, host } = seeded();
    const s = await createSession(host, "document", {
      model: "m",
      grant: DOCS_GRANT,
      max_iterations: 1,
    });
    f.replyCalls(["act__builtin__document__search_documents", { q: "x" }]);
    await step(host, s);
    expect((await step(host, s)).status).toBe("exhausted");

    await addTurn(host, s, "keep going");
    expect(s.turn_iteration, "the per-turn counter resets").toBe(0);
    expect(s.iteration, "the audit trail does not").toBe(1);
    expect(s.status).toBe("running");
  });

  test("requires a message", async () => {
    const { host } = seeded();
    const s = await session(host);
    await expect(addTurn(host, s, "")).rejects.toThrow(/message/);
  });
});

/**
 * The fix for the goal-shaped session. Under the old model the catalogue was
 * frozen at the first message, so a conversation that moved on could never
 * reach anything new and had to be restarted.
 */
describe("the catalogue follows the conversation", () => {
  // Note the terse second messages: the fake host stands in for FTS5 with a
  // term-AND match, so "now email that to me" would match nothing on any
  // ranking. Real FTS ranks; the behaviour under test is the re-resolution.
  test("re-resolves against the newest message, not the first", async () => {
    const { f, host } = seeded();
    withMail(f);
    const s = await createSession(host, "document", {
      model: "m",
      grant: [{ path: "/builtin/document" }, { path: "/packages/solx-google" }],
    });
    // The first message selected document tools; mail was not among them.
    expect(Object.values(s.tools)).not.toContain("/packages/solx-google/send-gmail-message");

    await addTurn(host, s, "email");
    expect(Object.values(s.tools)).toContain("/packages/solx-google/send-gmail-message");
  });

  test("a message that matches nothing falls back to the grant, not to nothing", async () => {
    // Caught by the live run: "list files" against a documents-only grant
    // resolved to zero tools, leaving the model unable to do or explain
    // anything. It falls back to the grant unfiltered instead.
    const { host } = seeded();
    const s = await session(host);
    const before = Object.keys(s.tools).length;
    expect(before).toBeGreaterThan(0);

    await addTurn(host, s, "xyzzy nothing matches this");
    expect(Object.keys(s.tools).length).toBeGreaterThan(0);
    expect(Object.values(s.tools)).toContain("/builtin/document/search_documents");
  });

  test("the fallback is a re-resolve, so a revoked path cannot linger", async () => {
    const { f, host } = seeded();
    withMail(f);
    const s = await createSession(host, "email", {
      model: "m",
      grant: [{ path: "/builtin/document" }, { path: "/packages/solx-google" }],
    });
    expect(Object.values(s.tools)).toContain("/packages/solx-google/send-gmail-message");

    // Revoke it, then send a message that matches nothing. The fallback must
    // resolve against the *current* grant rather than reuse the last list.
    await widenGrant(host, s, DOCS_GRANT);
    await addTurn(host, s, "xyzzy nothing matches this");
    expect(Object.values(s.tools)).not.toContain("/packages/solx-google/send-gmail-message");
    expect(Object.keys(s.tools).length).toBeGreaterThan(0);
  });

  test("but talking never widens what may be called", async () => {
    const { f, host } = seeded();
    withMail(f);
    // Mail is outside the grant this time.
    const s = await session(host);
    await addTurn(host, s, "email");
    expect(Object.values(s.tools)).not.toContain("/packages/solx-google/send-gmail-message");

    // And even if the map were tampered with, dispatch refuses it.
    s.tools["act__packages__solx_google__send_gmail_message"] =
      "/packages/solx-google/send-gmail-message";
    f.replyCalls(["act__packages__solx_google__send_gmail_message", {}]);
    await step(host, s);
    expect(s.calls[0].outcome).toBe("refused");
    expect(f.refsCalled("/packages/solx-google/send-gmail-message").length).toBe(0);
  });
});

describe("widening the grant", () => {
  test("is an operator action, announced in the transcript", async () => {
    const { f, host } = seeded();
    withMail(f);
    const s = await session(host);

    await widenGrant(host, s, [...DOCS_GRANT, { path: "/packages/solx-google" }]);
    const announced = s.messages.at(-1)!;
    expect(announced.role).toBe("system");
    expect(announced.content).toMatch(/solx-google/);

    // And the next turn can actually reach it.
    await addTurn(host, s, "email");
    expect(Object.values(s.tools)).toContain("/packages/solx-google/send-gmail-message");
  });

  test("is persisted, so a reload sees the same reach", async () => {
    const { host } = seeded();
    const s = await session(host);
    await widenGrant(host, s, [...DOCS_GRANT, { path: "/packages/solx-google" }]);

    const reloaded = await loadSession(host, s.id);
    expect(reloaded.grant.map((r) => r.path)).toContain("/packages/solx-google");
  });

  test("still refuses an empty grant", async () => {
    const { host } = seeded();
    const s = await session(host);
    await expect(widenGrant(host, s, [])).rejects.toThrow(/default-deny/);
  });

  test("narrowing does not announce anything, and takes effect next turn", async () => {
    const { f, host } = seeded();
    withMail(f);
    const s = await createSession(host, "document", {
      model: "m",
      grant: [{ path: "/builtin/document" }, { path: "/packages/solx-google" }],
    });
    const before = s.messages.length;
    await widenGrant(host, s, DOCS_GRANT);
    expect(s.messages.length, "nothing gained, nothing to announce").toBe(before);

    await addTurn(host, s, "email");
    expect(Object.values(s.tools)).not.toContain("/packages/solx-google/send-gmail-message");
  });
});

describe("previewTools", () => {
  test("resolves what a turn would get, and creates nothing", async () => {
    const { f, host } = seeded();
    const before = f.docs.size;
    const preview = await previewTools(host, DOCS_GRANT, "document", 10);
    expect(preview.tools.length).toBeGreaterThan(0);
    expect(Object.values(preview.refs)).toContain("/builtin/document/search_documents");
    expect(f.docs.size, "no session was created").toBe(before);
  });

  test("is default-deny too", async () => {
    const { host } = seeded();
    await expect(previewTools(host, [], "document", 10)).rejects.toThrow(/default-deny/);
  });
});

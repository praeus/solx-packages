/**
 * session.ts tests: naming, listing, round-tripping a session document, and
 * the stored-turn -> transcript mapping — exercised against the fake client,
 * no React tree or browser.
 */
import { describe, expect, it } from "vitest";
import { hostFromClient } from "../../solx-widgets/src/wrap/host";
import {
  deleteSession,
  listSessions,
  loadSessionTranscript,
  randomSessionName,
  saveSessionDocument,
} from "../src/session";
import type { XPromptSessionDocument } from "../src/types";
import { createFakeClient } from "./fakeClient";

function sessionDoc(name: string, turns: XPromptSessionDocument["contents"]["turns"]): XPromptSessionDocument {
  return {
    path: "/xprompt/sessions",
    name,
    typeRef: "/packages/solx-inquiry/MultiInquireSession",
    author: "/packages/solx-inquiry/multi_inquire",
    title: turns[0]?.instruction ?? "untitled",
    summary: turns[turns.length - 1]?.responses[0]?.text ?? "",
    contents: { turns, turnCount: turns.length, lastInstruction: turns[turns.length - 1]?.instruction ?? "" },
  };
}

describe("randomSessionName", () => {
  it("asks the random-name action and returns its name", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    const a = await randomSessionName(host);
    const b = await randomSessionName(host);
    expect(a).toBeTruthy();
    expect(b).toBeTruthy();
    expect(a).not.toBe(b);
  });
});

describe("saveSessionDocument / loadSessionTranscript", () => {
  it("reads back an empty transcript for a session nobody has saved yet", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    const turns = await loadSessionTranscript(host, "never-saved");
    expect(turns).toEqual([]);
  });

  it("round-trips a direct-mode turn into a user + answer pair", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    const doc = sessionDoc("s1", [
      {
        instruction: "what is 2+2?",
        mode: "direct",
        inquiries: [],
        responses: [{ text: "4", title: null, memory: false, tags: [], citations: [], inquiry: null }],
        scripts: [],
        memories: [],
        next_prompt: null,
        notes: [],
        errors: [],
      },
    ]);

    await saveSessionDocument(host, doc);
    const turns = await loadSessionTranscript(host, "s1");

    expect(turns).toHaveLength(2);
    expect(turns[0]).toMatchObject({ kind: "user", text: "what is 2+2?" });
    expect(turns[1]).toMatchObject({ kind: "answer" });
    if (turns[1].kind === "answer") {
      expect(turns[1].result.intent.mode).toBe("direct");
      expect(turns[1].result.responses[0].text).toBe("4");
      expect(turns[1].result.hits).toEqual([]);
    }
  });

  it("round-trips multiple turns in order", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    const doc = sessionDoc("s2", [
      {
        instruction: "first",
        mode: "direct",
        inquiries: [],
        responses: [{ text: "one", title: null, memory: false, tags: [], citations: [], inquiry: null }],
        scripts: [],
        memories: [],
        next_prompt: null,
        notes: [],
        errors: [],
      },
      {
        instruction: "second",
        mode: "inquire",
        inquiries: [{ kind: "documents", question: "second", terms: ["second"] }],
        responses: [{ text: "two", title: null, memory: false, tags: [], citations: ["/a"], inquiry: 0 }],
        scripts: [],
        memories: [],
        next_prompt: null,
        notes: [],
        errors: [],
      },
    ]);

    await saveSessionDocument(host, doc);
    const turns = await loadSessionTranscript(host, "s2");

    expect(turns.map((t) => (t.kind === "user" ? t.text : t.kind))).toEqual(["first", "answer", "second", "answer"]);
    const second = turns[3];
    if (second.kind === "answer") {
      expect(second.result.intent.mode).toBe("inquire");
      expect(second.result.responses[0].citations).toEqual(["/a"]);
    }
  });
});

describe("listSessions", () => {
  it("lists nothing before any session is saved", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    expect(await listSessions(host)).toEqual({ sessions: [], total: 0 });
  });

  it("lists saved sessions, most recently updated first", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    await saveSessionDocument(host, sessionDoc("older", []));
    await saveSessionDocument(host, sessionDoc("newer", []));

    const { sessions, total } = await listSessions(host);
    expect(sessions.map((s) => s.name)).toEqual(["newer", "older"]);
    expect(total).toBe(2);
  });

  it("does not pick up documents outside the sessions path", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    await saveSessionDocument(host, sessionDoc("real-session", []));
    await host.call("/builtin/document/entity-save-document", {
      path: "/xprompt/call-logs",
      name: "real-session",
      contents: { calls: [] },
    });

    const { sessions } = await listSessions(host);
    expect(sessions.map((s) => s.name)).toEqual(["real-session"]);
  });
});

describe("deleteSession", () => {
  const callLog = (name: string) => ({
    path: "/xprompt/call-logs",
    name,
    contents: { calls: [] },
  });

  it("removes both the session document and its call log", async () => {
    const { client, docKeys } = createFakeClient();
    const host = hostFromClient(client);
    await saveSessionDocument(host, sessionDoc("doomed", []));
    await host.call("/builtin/document/entity-save-document", callLog("doomed"));

    await deleteSession(host, "doomed");

    expect(docKeys()).toEqual([]);
  });

  it("leaves other sessions alone", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    await saveSessionDocument(host, sessionDoc("keep", []));
    await saveSessionDocument(host, sessionDoc("doomed", []));

    await deleteSession(host, "doomed");

    const { sessions } = await listSessions(host);
    expect(sessions.map((s) => s.name)).toEqual(["keep"]);
  });

  it("succeeds for a session that never had a call log", async () => {
    // The normal case: a session is saved after its first turn, but the call
    // log only exists once something was tracked under it.
    const { client, docKeys } = createFakeClient();
    const host = hostFromClient(client);
    await saveSessionDocument(host, sessionDoc("no-log", []));

    await expect(deleteSession(host, "no-log")).resolves.toBeUndefined();
    expect(docKeys()).toEqual([]);
  });

  it("throws when the session document itself cannot be deleted", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    await expect(deleteSession(host, "never-existed")).rejects.toThrow();
  });

  it("deletes the call log before the session document", async () => {
    // Order is the contract: a failure partway must leave the session still
    // listed and the operation retryable, never a call log stranded behind a
    // session that has vanished from the picker.
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    await saveSessionDocument(host, sessionDoc("ordered", []));
    await host.call("/builtin/document/entity-save-document", callLog("ordered"));

    const seen: string[] = [];
    const original = client.actions.exec.bind(client.actions);
    client.actions.exec = async (path, name, params) => {
      if (name === "entity-delete-document") {
        seen.push(String((params as Record<string, unknown>).path));
      }
      return original(path, name, params);
    };

    await deleteSession(host, "ordered");
    expect(seen).toEqual(["/xprompt/call-logs", "/xprompt/sessions"]);
  });

  it("does nothing for an empty name", async () => {
    const { client, docKeys } = createFakeClient();
    const host = hostFromClient(client);
    await saveSessionDocument(host, sessionDoc("safe", []));

    await deleteSession(host, "");

    expect(docKeys()).toHaveLength(1);
  });
});

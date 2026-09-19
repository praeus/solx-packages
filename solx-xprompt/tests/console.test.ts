import { describe, expect, it } from "vitest";
import { hostFromClient } from "../../solx-widgets/src/wrap/host";
import { appendCall, loadCallLog, startTrackedCall } from "../src/console/callLog";
import { readMergedConsole } from "../src/console/merge";
import { createFakeClient } from "./fakeClient";

describe("call log", () => {
  it("loads as empty when nothing has been tracked yet", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);
    const log = await loadCallLog(host, "session-1");
    expect(log.calls).toEqual([]);
  });

  it("records a tracked call and reads it back", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);

    const inv = await startTrackedCall(client, host, "session-1", "/packages/solx-ollama", "ollama-chat", {
      model: "llama3.2:1b",
    });

    const log = await loadCallLog(host, "session-1");
    expect(log.calls).toHaveLength(1);
    expect(log.calls[0]).toMatchObject({
      actionRef: inv.action_ref,
      invocationId: inv.invocation_id,
      consoleSeqStart: inv.console_seq_start,
    });
  });

  it("accumulates calls across separate appends rather than overwriting", async () => {
    const { client } = createFakeClient();
    const host = hostFromClient(client);

    await appendCall(host, "session-1", {
      actionRef: "/a",
      name: "a",
      invocationId: "inv-1",
      consoleSeqStart: 1,
      startedAt: new Date().toISOString(),
    });
    await appendCall(host, "session-1", {
      actionRef: "/b",
      name: "b",
      invocationId: "inv-2",
      consoleSeqStart: 1,
      startedAt: new Date().toISOString(),
    });

    const log = await loadCallLog(host, "session-1");
    expect(log.calls.map((c) => c.actionRef)).toEqual(["/a", "/b"]);
  });
});

describe("merged console", () => {
  it("returns nothing for a log with no calls", async () => {
    const { client } = createFakeClient();
    const { entries } = await readMergedConsole(client, { calls: [] });
    expect(entries).toEqual([]);
  });

  it("merges entries from two different actions into one time-ordered view", async () => {
    const { client, pushEntry } = createFakeClient();
    const host = hostFromClient(client);

    const chat = await startTrackedCall(client, host, "session-1", "/packages/solx-ollama", "ollama-chat", {});
    const search = await startTrackedCall(client, host, "session-1", "/builtin/document", "search-documents", {});

    pushEntry(chat.action_ref, chat.invocation_id, "chat: thinking");
    pushEntry(search.action_ref, search.invocation_id, "search: querying");
    pushEntry(chat.action_ref, chat.invocation_id, "chat: done");

    const log = await loadCallLog(host, "session-1");
    const { entries } = await readMergedConsole(client, log);

    expect(entries.map((e) => e.message)).toEqual(["chat: thinking", "search: querying", "chat: done"]);
    expect(entries.map((e) => e.actionRef)).toEqual([chat.action_ref, search.action_ref, chat.action_ref]);
  });

  it("filters out a concurrent, unrelated invocation of the same action", async () => {
    const { client, pushEntry } = createFakeClient();
    const host = hostFromClient(client);

    // Two separate widget sessions both call the same action.
    const mine = await startTrackedCall(client, host, "session-mine", "/packages/solx-ollama", "ollama-chat", {});
    const theirs = await client.invocations.start("/packages/solx-ollama", "ollama-chat", {});

    pushEntry(mine.action_ref, theirs.invocation_id, "not mine");
    pushEntry(mine.action_ref, mine.invocation_id, "mine");

    const log = await loadCallLog(host, "session-mine");
    const { entries } = await readMergedConsole(client, log);

    expect(entries.map((e) => e.message)).toEqual(["mine"]);
  });

  it("resumes from returned cursors instead of re-reading old entries", async () => {
    const { client, pushEntry } = createFakeClient();
    const host = hostFromClient(client);

    const chat = await startTrackedCall(client, host, "session-1", "/packages/solx-ollama", "ollama-chat", {});
    pushEntry(chat.action_ref, chat.invocation_id, "first");

    const log = await loadCallLog(host, "session-1");
    const first = await readMergedConsole(client, log);
    expect(first.entries.map((e) => e.message)).toEqual(["first"]);

    pushEntry(chat.action_ref, chat.invocation_id, "second");
    const second = await readMergedConsole(client, log, first.cursors);
    expect(second.entries.map((e) => e.message)).toEqual(["second"]);
  });

  it("orders entries sharing a timestamp by seq", async () => {
    // `ts` is a server-side `Utc::now()`, and a burst of milestones emitted
    // back to back with no I/O between them genuinely shares one - three
    // `inquiry.started` events, say. Emission order is what the progress fold
    // depends on, and within one console that is `seq`.
    const { client, pushEntry } = createFakeClient();
    const host = hostFromClient(client);

    const call = await startTrackedCall(client, host, "session-1", "/packages/solx-inquiry", "multi-inquire", {});
    const ts = "2026-01-01T00:00:00.000Z";
    for (const message of ["first", "second", "third"]) {
      pushEntry(call.action_ref, call.invocation_id, message, { ts });
    }

    const log = await loadCallLog(host, "session-1");
    const { entries } = await readMergedConsole(client, log);
    expect(entries.map((e) => e.message)).toEqual(["first", "second", "third"]);
  });

  it("carries the structured data half through the merge", async () => {
    // The machine-readable half is the whole point of the progress feed; a
    // merge that dropped it would leave the strip with nothing to read.
    const { client, pushEntry } = createFakeClient();
    const host = hostFromClient(client);

    const call = await startTrackedCall(client, host, "session-1", "/packages/solx-inquiry", "multi-inquire", {});
    pushEntry(call.action_ref, call.invocation_id, "[multi_inquire:fanout] 3 inquiries running", {
      data: { ev: { v: 1, t: "fanout.started", n: 3 } },
    });

    const log = await loadCallLog(host, "session-1");
    const { entries } = await readMergedConsole(client, log);
    expect(entries[0].data).toEqual({ ev: { v: 1, t: "fanout.started", n: 3 } });
  });
});

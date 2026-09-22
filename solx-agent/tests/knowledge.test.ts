/** Memories, context documents and skills -- and the confinement of each. */

import { describe, expect, test } from "vitest";
import { createSession } from "../src/harness/agent";
import { step } from "../src/harness/turn";
import { sysToolDefs } from "../src/harness/sysTools";
import { DOCS_GRANT, fake, withDocActions } from "./fakeHost";
import type { Host } from "../src/harness/host";

const MEMORY_TYPE = "/packages/solx-agent/AgentMemory";
const SKILL_TYPE = "/packages/solx-agent/AgentSkill";

function seeded() {
  const { fake: f, host } = fake();
  withDocActions(f);
  return { f, host };
}

async function session(host: Host, opts: Record<string, unknown> = {}) {
  return createSession(host, "document", { model: "m", grant: DOCS_GRANT, ...opts });
}

/**
 * A realistic message, as opposed to the one-word "document" above.
 *
 * This distinction used to be the whole ballgame: skills were searched with
 * the user message as a full-text query, and solx-docs ANDs every
 * whitespace-separated term, so anything longer than a word or two matched
 * nothing and no skill ever loaded. The fake host reproduces that AND
 * faithfully -- the bug survived because every fixture here was one word.
 */
const SENTENCE = "find the documents about widgets and summarize them for me";

async function sessionSaying(host: Host, message: string, opts: Record<string, unknown> = {}) {
  return createSession(host, message, { model: "m", grant: DOCS_GRANT, ...opts });
}

describe("memory", () => {
  test("is off unless a scope is given, and a bad scope is an error", async () => {
    const { host } = seeded();
    const s = await session(host);
    expect(s.memory).toBeNull();
    // Never told it exists.
    expect(sysToolDefs(s).map((d) => d.function.name)).not.toContain("sys__memory_search");

    await expect(session(host, { memory: { scope: "a/b" } })).rejects.toThrow(/path segment/);
  });

  test("a saved memory lands under its scope with the text in summary", async () => {
    const { f, host } = seeded();
    const s = await session(host, { memory: { scope: "blogs" } });
    f.replyCalls(["sys__memory_save", { text: "search before saving", tags: ["docs"] }]);
    await step(host, s);

    const saved = [...f.docs.values()].find((d) => d.typeRef === MEMORY_TYPE);
    expect(saved).toBeTruthy();
    expect(saved!.path).toBe("/agent/memories/blogs");
    // The text is in `summary` as well as contents -- that is what makes
    // recall one search with no follow-up reads.
    expect(saved!.summary).toBe("search before saving");
    expect(saved!.contents.text).toBe("search before saving");
    expect(s.memories_written).toBe(1);
  });

  test("recall needs one search and no gets, and is framed as reference", async () => {
    const { f, host } = seeded();
    f.doc("/agent/memories/blogs/mem-a", {
      typeRef: MEMORY_TYPE,
      summary: "documents live under /blogs",
    });
    const s = await session(host, { memory: { scope: "blogs" } });

    // A memory that fits in `summary` is fully carried by the search hit, so
    // recall costs one search and no follow-up reads. (Creating a session
    // does get one document of its own -- the name-collision check -- so the
    // claim is about the memory path, not the total.)
    const gets = f
      .refsCalled("/builtin/document/entity-get-document")
      .filter((c) => String(c.params.path ?? "").startsWith("/agent/memories"));
    expect(gets.length).toBe(0);
    const seededTurn = s.messages.find(
      (m) => m.role === "system" && m.content.includes("documents live under /blogs"),
    );
    expect(seededTurn).toBeTruthy();
    expect(seededTurn!.content).toMatch(/not an instruction/);
    expect(seededTurn!.content).toMatch(/never widens/);
  });

  test("the write budget is enforced", async () => {
    const { f, host } = seeded();
    const s = await session(host, { memory: { scope: "blogs", max_writes: 1 } });
    f.replyCalls(["sys__memory_save", { text: "one" }]);
    await step(host, s);
    f.replyCalls(["sys__memory_save", { text: "two" }]);
    await step(host, s);

    expect(s.memories_written).toBe(1);
    expect(s.messages.at(-1)!.content).toMatch(/limit reached/);
  });

  test("memory_save is refused when memory is off", async () => {
    const { f, host } = seeded();
    const s = await session(host);
    f.replyCalls(["sys__memory_save", { text: "x" }]);
    await step(host, s);
    expect(s.messages.at(-1)!.content).toMatch(/not enabled/);
  });
});

describe("context", () => {
  test("resolves by ref and by query, and is frozen into the session", async () => {
    const { f, host } = seeded();
    f.doc("/specs/house-style", { title: "House style", summary: "how we write" });
    f.doc("/docs/onboarding", { title: "Onboarding", summary: "document the setup" });

    const s = await session(host, {
      context: [{ ref: "/specs/house-style" }, { query: "document", path: "/docs", limit: 3 }],
    });
    const refs = s.context.map((c) => c.ref);
    expect(refs).toContain("/specs/house-style");
    expect(refs).toContain("/docs/onboarding");
  });

  test("a missing ref is an error at creation, not a silent gap", async () => {
    const { host } = seeded();
    await expect(session(host, { context: [{ ref: "/nope/missing" }] })).rejects.toThrow(
      /not found/,
    );
  });

  test("context_read opens what is listed and refuses what is not", async () => {
    const { f, host } = seeded();
    f.doc("/specs/house-style", { title: "House style", contents: { body: "sentence case" } });
    f.doc("/secrets/keys", { title: "Keys", contents: { body: "hunter2" } });
    const s = await session(host, { context: [{ ref: "/specs/house-style" }] });

    f.replyCalls(["sys__context_read", { ref: "/specs/house-style" }]);
    await step(host, s);
    expect(s.messages.at(-1)!.content).toMatch(/sentence case/);

    f.replyCalls(["sys__context_read", { ref: "/secrets/keys" }]);
    await step(host, s);
    expect(s.messages.at(-1)!.content).toMatch(/no context document/);
    expect(s.messages.at(-1)!.content).not.toMatch(/hunter2/);
  });

  test("context_read is offered only when there is context", async () => {
    const { f, host } = seeded();
    const bare = await session(host);
    expect(sysToolDefs(bare).map((d) => d.function.name)).not.toContain("sys__context_read");

    f.doc("/specs/house-style", { title: "House style" });
    const withCtx = await session(host, { context: [{ ref: "/specs/house-style" }] });
    expect(sysToolDefs(withCtx).map((d) => d.function.name)).toContain("sys__context_read");
  });
});

describe("skills", () => {
  const skill = (tools: string[], instructions: string) => ({
    typeRef: SKILL_TYPE,
    title: "Documents",
    contents: { tools, instructions },
  });

  test("load when a glob matches something in the catalogue", async () => {
    const { f, host } = seeded();
    f.doc("/agent/skills/documents", skill(["/builtin/document/*"], "Search before saving."));
    const s = await session(host);
    const injected = s.messages.find(
      (m) => m.role === "system" && m.content.includes("Search before saving."),
    );
    expect(injected).toBeTruthy();
  });

  test("stay out when their glob matches nothing in the catalogue", async () => {
    const { f, host } = seeded();
    f.doc("/agent/skills/media", skill(["/packages/solx-media/*"], "Transcode carefully."));
    const s = await session(host);
    expect(s.messages.some((m) => m.content.includes("Transcode carefully."))).toBe(false);
  });

  test("can be switched off", async () => {
    const { f, host } = seeded();
    f.doc("/agent/skills/documents", skill(["/builtin/document/*"], "Search before saving."));
    const s = await session(host, { skills: { enabled: false } });
    expect(s.messages.some((m) => m.content.includes("Search before saving."))).toBe(false);
  });

  test("load for a message no skill text could ever match word for word", async () => {
    const { f, host } = seeded();
    f.doc("/agent/skills/documents", skill(["/builtin/document/*"], "Search before saving."));
    const s = await sessionSaying(host, SENTENCE);
    const injected = s.messages.find(
      (m) => m.role === "system" && m.content.includes("Search before saving."),
    );
    expect(injected).toBeTruthy();
  });

  test("an oversized skill does not starve the ones after it", async () => {
    const { f, host } = seeded();
    // Over SKILL_TOTAL_CAP on its own, so it can never fit the budget.
    f.doc("/agent/skills/a-huge", skill(["/builtin/document/*"], "H".repeat(9000)));
    f.doc("/agent/skills/z-small", skill(["/builtin/document/*"], "Search before saving."));
    const s = await sessionSaying(host, SENTENCE);
    expect(s.messages.some((m) => m.content.includes("Search before saving."))).toBe(true);
  });

  test("resolve from the search alone, with no follow-up read per candidate", async () => {
    const { f, host } = seeded();
    f.doc("/agent/skills/documents", skill(["/builtin/document/*"], "Search before saving."));
    f.doc("/agent/skills/media", skill(["/packages/solx-media/*"], "Transcode carefully."));
    const s = await session(host);
    expect(s.messages.some((m) => m.content.includes("Search before saving."))).toBe(true);
    // `search-documents` returns whole documents, so the globs and the
    // instructions are already in hand. A get aimed at the skills path means
    // the N+1 round-trip has come back.
    const gets = f
      .refsCalled("/builtin/document/entity-get-document")
      .filter((c) => String((c.params as { path?: string }).path || "").startsWith("/agent/skills"));
    expect(gets).toEqual([]);
  });

  test("a skill without tools or instructions is ignored", async () => {
    const { f, host } = seeded();
    f.doc("/agent/skills/empty", { typeRef: SKILL_TYPE, contents: { tools: [], instructions: "" } });
    const s = await session(host);
    expect(s.skills_seen).toEqual({});
  });
});

describe("tool_search", () => {
  test("widens what is visible, inside the grant", async () => {
    const { f, host } = seeded();
    f.action("/builtin/document/entity-delete-document", { description: "delete a document" });
    const s = await createSession(host, "search", { model: "m", grant: DOCS_GRANT });
    const before = Object.keys(s.tools).length;

    f.replyCalls(["sys__tool_search", { q: "delete" }]);
    await step(host, s);
    expect(Object.keys(s.tools).length).toBeGreaterThan(before);
    expect(Object.values(s.tools)).toContain("/builtin/document/entity-delete-document");
  });

  test("cannot reach past the grant, but says where the match was", async () => {
    const { f, host } = seeded();
    f.action("/packages/solx-google/send-gmail-message", { description: "send an email" });
    const s = await session(host);

    f.replyCalls(["sys__tool_search", { q: "email" }]);
    await step(host, s);
    expect(Object.values(s.tools)).not.toContain("/packages/solx-google/send-gmail-message");

    // Naming the path is what turns a dead end into something the model can
    // hand back to the operator. Without it, "nothing matched" reads as
    // "search again" and the model starts guessing at tool names.
    const said = s.messages.at(-1)!.content;
    expect(said).toContain("/packages/solx-google");
    // Paths only: a name it cannot call is what starts the guessing.
    expect(said).not.toContain("send-gmail-message");
  });

  test("a search that matches nothing anywhere does not blame the grant", async () => {
    const { f, host } = seeded();
    const s = await session(host);

    f.replyCalls(["sys__tool_search", { q: "zzzznothingmatchesthis" }]);
    await step(host, s);
    const said = s.messages.at(-1)!.content;
    expect(said).toMatch(/no further tools matched/);
    expect(said).not.toMatch(/do exist at/);
  });

  test("evicts to make room at the catalogue cap, rather than dead-ending", async () => {
    const { f, host } = seeded();
    f.action("/builtin/document/entity-delete-document", { description: "document delete" });
    const s = await createSession(host, "document", {
      model: "m",
      grant: DOCS_GRANT,
      catalogue_cap: 2,
    });
    expect(Object.keys(s.tools).length).toBe(2);
    const before = Object.keys(s.tools);

    // The search surfaces a tool not already held; the cap is full, so a held
    // tool is evicted to make room rather than the search being refused with
    // "no room for more tools".
    f.replyCalls(["sys__tool_search", { q: "delete" }]);
    await step(host, s);

    expect(Object.keys(s.tools).length).toBe(2);
    expect(s.messages.at(-1)!.content).toMatch(/dropped from the catalogue/);

    // The point of the change: the tool the search just found is *held*, and
    // something that was there before is gone. Counting alone passed happily
    // against the old "refuse when full" behaviour, so it proved nothing.
    const after = Object.keys(s.tools);
    const added = after.filter((n) => !before.includes(n));
    const gone = before.filter((n) => !after.includes(n));
    expect(added.length).toBeGreaterThan(0);
    expect(gone.length).toBe(added.length);
    for (const n of added) expect(s.tools_defs.some((d) => d.function.name === n)).toBe(true);
    for (const n of gone) expect(s.tools_defs.some((d) => d.function.name === n)).toBe(false);
  });

  test("never evicts a tool an immediately preceding search added", async () => {
    const { f, host } = seeded();
    f.action("/builtin/document/entity-delete-document", { description: "document delete" });
    f.action("/builtin/document/entity-list-documents", { description: "document listing" });
    const s = await createSession(host, "document", {
      model: "m",
      grant: DOCS_GRANT,
      catalogue_cap: 2,
    });

    // The regression this guards: eviction used to walk insertion order, so
    // once the initial catalogue was gone the previous search's results were
    // the oldest entries and consecutive searches cannibalised each other. A
    // real session lost `file-put` one search after fetching it and died
    // refused. Two searches in a row must leave the second one's tools held.
    f.replyCalls(["sys__tool_search", { q: "delete" }]);
    await step(host, s);
    const deleteTool = "act__builtin__document__entity-delete-document";
    expect(Object.keys(s.tools)).toContain(deleteTool);

    f.replyCalls(["sys__tool_search", { q: "listing" }]);
    await step(host, s);

    const held = Object.keys(s.tools);
    expect(held.length).toBe(2);
    // Both searches' finds are held. Under insertion order the second search
    // would have evicted the first one's, because by then it was the oldest.
    expect(held).toContain(deleteTool);
    expect(held).toContain("act__builtin__document__entity-list-documents");
  });

  test("keeps a tool that was called over one that was only ever listed", async () => {
    const { f, host } = seeded();
    f.action("/builtin/document/entity-delete-document", { description: "document delete" });
    const s = await createSession(host, "document", {
      model: "m",
      grant: DOCS_GRANT,
      catalogue_cap: 2,
    });
    const [first, second] = Object.keys(s.tools);

    // Use `first`, so it is the more recently touched of the two.
    f.replyCalls([first, {}]);
    await step(host, s);

    f.replyCalls(["sys__tool_search", { q: "delete" }]);
    await step(host, s);

    // `second` was never called and is the stalest thing held, so it is what
    // goes. Under the old insertion-order rule `first` would have gone instead,
    // precisely because it was added first.
    const held = Object.keys(s.tools);
    expect(held).toContain(first);
    expect(held).not.toContain(second);
  });

  test("is offered by default and can be switched off", async () => {
    const { host } = seeded();
    const on = await session(host);
    expect(sysToolDefs(on).map((d) => d.function.name)).toContain("sys__tool_search");

    const off = await session(host, { tool_search: false });
    expect(sysToolDefs(off).map((d) => d.function.name)).not.toContain("sys__tool_search");
  });
});

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
      .refsCalled("/builtin/document/entity_get_document")
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
    f.action("/builtin/document/entity_delete_document", { description: "delete a document" });
    const s = await createSession(host, "search", { model: "m", grant: DOCS_GRANT });
    const before = Object.keys(s.tools).length;

    f.replyCalls(["sys__tool_search", { q: "delete" }]);
    await step(host, s);
    expect(Object.keys(s.tools).length).toBeGreaterThan(before);
    expect(Object.values(s.tools)).toContain("/builtin/document/entity_delete_document");
  });

  test("cannot reach past the grant", async () => {
    const { f, host } = seeded();
    f.action("/packages/solx-google/send-gmail-message", { description: "send an email" });
    const s = await session(host);

    f.replyCalls(["sys__tool_search", { q: "email" }]);
    await step(host, s);
    expect(Object.values(s.tools)).not.toContain("/packages/solx-google/send-gmail-message");
  });

  test("stops at the catalogue cap", async () => {
    const { f, host } = seeded();
    f.action("/builtin/document/entity_delete_document", { description: "document delete" });
    const s = await createSession(host, "document", {
      model: "m",
      grant: DOCS_GRANT,
      catalogue_cap: 2,
    });
    expect(Object.keys(s.tools).length).toBe(2);

    f.replyCalls(["sys__tool_search", { q: "document" }]);
    await step(host, s);
    expect(Object.keys(s.tools).length).toBe(2);
    expect(s.messages.at(-1)!.content).toMatch(/maximum/);
  });

  test("is offered by default and can be switched off", async () => {
    const { host } = seeded();
    const on = await session(host);
    expect(sysToolDefs(on).map((d) => d.function.name)).toContain("sys__tool_search");

    const off = await session(host, { tool_search: false });
    expect(sysToolDefs(off).map((d) => d.function.name)).not.toContain("sys__tool_search");
  });
});

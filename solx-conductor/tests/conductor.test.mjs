import test from "node:test";
import assert from "node:assert/strict";
import { load, FakeHost } from "./harness.mjs";

const { __internals: G } = await load();

const SESSION_TYPE = "/packages/solx-conductor/ConductorSession";
const MEMORY_TYPE = "/packages/solx-conductor/ConductorMemory";
const SKILL_TYPE = "/packages/solx-conductor/ConductorSkill";

/** A host with a few document actions and one command, ready for `start`. */
function host() {
  return new FakeHost()
    .action("/builtin/document/search_documents", { description: "search documents" })
    .action("/builtin/document/entity_get_document", { description: "get a document" })
    .action("/builtin/document/entity_save_document", { description: "save a document" })
    .action("/builtin/document/set_field_at_path", { description: "set one document field" });
}

const DOCS_ONLY = [{ path: "/builtin/document" }];

// ── the gate ────────────────────────────────────────────────────────────────

test("allow is default-deny: absent and empty are errors, not empty catalogues", () => {
  for (const bad of [undefined, null, []]) {
    assert.throws(() => G.normalizeAllow(bad), /default-deny/);
  }
  assert.throws(() => G.normalizeAllow([{ path: "" }]), /non-empty path/);
});

test("a glob reaches a script action but never a command", () => {
  const allow = [{ path: "/tools" }];
  const script = { path: "/tools", name: "a", actionType: "script" };
  const command = { path: "/tools", name: "sh", actionType: "command" };

  assert.equal(G.permitted(script, allow), true);
  assert.equal(G.permitted(command, allow), false, "a glob must never reach a shell");
  assert.equal(G.permitted(command, [{ path: "/tools", actions: ["sh"] }]), true);
  assert.equal(G.permitted(command, [{ path: "/tools", actions: ["other"] }]), false);
});

test("hard denies hold whatever the allowlist says", () => {
  const wideOpen = [{ path: "*", actions: ["get_secret", "entity_save_action", "start", "set_env"] }];
  for (const [path, name] of [
    ["/builtin/secrets", "get_secret"],
    ["/builtin/action", "entity_save_action"],
    ["/builtin/env", "set_env"],
    ["/packages/solx-conductor", "start"]
  ]) {
    assert.equal(
      G.permitted({ path, name, actionType: "internal" }, wideOpen), false,
      path + "/" + name + " must be refused"
    );
  }
});

test("conductor-owned document paths are refused, by whichever param names them", () => {
  const session = { skills: { path: "/conductor/skills" } };
  const SAVE = "/builtin/document/entity_save_document";
  const AT_PATH = "/builtin/document/set_field_at_path";

  assert.match(G.reservedDocWrite(session, SAVE, { path: "/conductor/sessions" }), /conductor-owned/);
  assert.match(G.reservedDocWrite(session, SAVE, { path: "/conductor/memories/proj" }), /conductor-owned/);
  assert.match(G.reservedDocWrite(session, SAVE, { path: "/conductor/skills" }), /conductor-owned/);
  assert.equal(G.reservedDocWrite(session, SAVE, { path: "/notes" }), null);

  // set_field_at_path names its entity path `doc_path`; its `path` is a JSON
  // pointer into contents. Reading the wrong key disables the check entirely.
  assert.match(
    G.reservedDocWrite(session, AT_PATH, { doc_path: "/conductor/sessions", path: "allow/0" }),
    /conductor-owned/
  );
  assert.equal(G.reservedDocWrite(session, AT_PATH, { doc_path: "/notes", path: "a" }), null);

  // A read is not a write, and an unrelated action is not a document writer.
  assert.equal(G.reservedDocWrite(session, "/builtin/document/entity_get_document",
                                  { path: "/conductor/sessions" }), null);
});

test("the reserved-path check is case-insensitive and survives a trailing slash", () => {
  const session = { skills: { path: "/conductor/skills" } };
  const SAVE = "/builtin/document/entity_save_document";
  assert.match(G.reservedDocWrite(session, SAVE, { path: "/Conductor/Sessions/" }), /conductor-owned/);
  assert.equal(G.isAtOrUnder("/conductor/memories/proj", "/conductor/memories"), true);
  assert.equal(G.isAtOrUnder("/conductor/memoriesx", "/conductor/memories"), false,
               "a sibling that merely shares a prefix is not underneath");
});

test("the default skills path stays reserved even when skills point elsewhere", () => {
  const session = { skills: { path: "/team/skills" } };
  const SAVE = "/builtin/document/entity_save_document";
  assert.match(G.reservedDocWrite(session, SAVE, { path: "/conductor/skills" }), /conductor-owned/);
  assert.match(G.reservedDocWrite(session, SAVE, { path: "/team/skills" }), /conductor-owned/);
});

test("a memory scope must be one path segment", () => {
  // Exactly solx_surface::path::validate_segment: empty, . and .., the three
  // separators, and control characters. A space is legal there, so it is here.
  for (const bad of ["", ".", "..", "a/b", "a\\b", "c:", "a\u0000b"]) {
    assert.equal(G.validSegment(bad), false, JSON.stringify(bad) + " must be rejected");
  }
  for (const good of ["proj", "my-project", "release_2026", "a b"]) {
    assert.equal(G.validSegment(good), true, good);
  }
});

// ── catalogue ───────────────────────────────────────────────────────────────

test("the catalogue search sends the keys solx actually reads", () => {
  // Regression: ActionSearchQuery is rename_all = "camelCase", and schemas are
  // open, so path_prefix/exclude_hidden are dropped in silence — an
  // unfiltered catalogue with no error anywhere.
  const h = host();
  G.resolveCatalogue("documents", DOCS_ONLY, 10, null);
  const [search] = h.refsCalled("/builtin/action/search_actions");
  assert.deepEqual(Object.keys(search.params).sort(), ["excludeHidden", "limit", "pathPrefix", "q"]);
  assert.equal(search.params.excludeHidden, true);
  assert.equal(search.params.pathPrefix, "/builtin/document");
});

test("hidden actions never enter the catalogue", () => {
  const h = host().action("/builtin/document/secret_doc_thing", {
    description: "document", capabilities: ["solx:hidden"]
  });
  const cat = G.resolveCatalogue("document", DOCS_ONLY, 10, null);
  assert.ok(!Object.values(cat.map).includes("/builtin/document/secret_doc_thing"));
  assert.ok(Object.values(cat.map).includes("/builtin/document/search_documents"));
  assert.ok(h);
});

test("the cap truncates and reports what it dropped", () => {
  host();
  const cat = G.resolveCatalogue("document", DOCS_ONLY, 2, null);
  assert.equal(cat.tools.length, 2);
  assert.equal(cat.dropped, 2, "four matched, two offered");
});

test("known refs are not re-offered", () => {
  host();
  const known = { "/builtin/document/search_documents": true };
  const cat = G.resolveCatalogue("document", DOCS_ONLY, 10, known);
  assert.ok(!Object.values(cat.map).includes("/builtin/document/search_documents"));
});

// ── start ───────────────────────────────────────────────────────────────────

test("start writes a session document with a typeRef", () => {
  // Regression: entity_save_document requires typeRef on create and ignores
  // an unknown type_ref, so the snake_case spelling made start fail outright.
  const h = host();
  const out = G.start({ model: "m", goal: "document", allow: DOCS_ONLY });
  const doc = h.docs.get("/conductor/sessions/" + out.session_id);
  assert.ok(doc, "session document was created");
  assert.equal(doc.typeRef, SESSION_TYPE);
  assert.equal(doc.contents.status, "running");
  assert.equal(out.status, "running");
  assert.ok(out.tools.length > 0);
});

test("start refuses a query that resolves to nothing", () => {
  host();
  assert.throws(
    () => G.start({ model: "m", goal: "nothing matches this", allow: DOCS_ONLY }),
    /no tools/
  );
});

// ── the loop ────────────────────────────────────────────────────────────────

/** start a session and return [host, sessionId]. */
function session(h, params = {}) {
  const out = G.start({ model: "m", goal: "document", allow: DOCS_ONLY, ...params });
  return out.session_id;
}

test("a tool call is dispatched and answered with exactly one tool turn", () => {
  const h = host();
  const id = session(h);
  h.replyCalls(["act__builtin__document__search_documents", { q: "x" }]);

  const out = G.step({ session_id: id });
  assert.equal(out.status, "running");
  const s = G.session({ session_id: id });
  const toolTurns = s.messages.filter((m) => m.role === "tool");
  assert.equal(toolTurns.length, 1);
  assert.equal(s.calls[0].outcome, "ok");
  assert.equal(s.calls[0].ref, "/builtin/document/search_documents");
});

test("a name absent from the session map names nothing", () => {
  const h = host();
  const id = session(h);
  h.replyCalls(["act__builtin__secrets__get_secret", {}]);

  G.step({ session_id: id });
  const s = G.session({ session_id: id });
  assert.equal(s.calls[0].outcome, "refused");
  assert.match(s.messages.at(-1).content, /unknown tool/);
});

test("no tool calls ends the session as final", () => {
  const h = host();
  const id = session(h);
  h.reply({ role: "assistant", content: "here is the answer" });
  const out = G.step({ session_id: id });
  assert.equal(out.status, "final");
  assert.equal(out.final, "here is the answer");
});

test("three iterations where every call fails end the session blocked", () => {
  // Deliberately not search_documents: the fake host answers that one itself,
  // so a failure injected on the row would never be reached.
  const h = host();
  h.actions.get("/builtin/document/set_field_at_path").failWith = "boom";
  const id = session(h);
  for (let i = 0; i < 3; i++) h.replyCalls(["act__builtin__document__set_field_at_path", { a: 1 }]);

  let out;
  for (let i = 0; i < 3; i++) out = G.step({ session_id: id });
  assert.equal(out.status, "blocked");
});

// ── approval ────────────────────────────────────────────────────────────────

test("a destructive call suspends, and an approval by call_id releases exactly it", () => {
  const h = host().action("/builtin/shell/run", { actionType: "command", description: "document shell" });
  const allow = [{ path: "/builtin/document" }, { path: "/builtin/shell", actions: ["run"] }];
  const id = G.start({ model: "m", goal: "document", allow }).session_id;

  h.replyCalls(["act__builtin__shell__run", { cmd: "rm -rf /" }]);
  const stopped = G.step({ session_id: id });
  assert.equal(stopped.status, "awaiting_approval");
  assert.equal(stopped.pending.length, 1);
  assert.equal(stopped.pending[0].ref, "/builtin/shell/run");
  assert.deepEqual(stopped.pending[0].arguments, { cmd: "rm -rf /" });
  assert.equal(h.refsCalled("/builtin/shell/run").length, 0, "nothing ran before approval");

  const done = G.step({ session_id: id, approve: [stopped.pending[0].call_id] });
  assert.equal(done.status, "running");
  assert.equal(h.refsCalled("/builtin/shell/run").length, 1);
});

test("resuming without naming a call denies it", () => {
  const h = host().action("/builtin/shell/run", { actionType: "command", description: "document shell" });
  const allow = [{ path: "/builtin/document" }, { path: "/builtin/shell", actions: ["run"] }];
  const id = G.start({ model: "m", goal: "document", allow }).session_id;

  h.replyCalls(["act__builtin__shell__run", { cmd: "x" }]);
  G.step({ session_id: id });
  G.step({ session_id: id, approve: [] });

  const s = G.session({ session_id: id });
  assert.equal(s.calls[0].outcome, "denied");
  assert.equal(h.refsCalled("/builtin/shell/run").length, 0);
});

// ── dispatch-time gate ──────────────────────────────────────────────────────

test("a tampered dispatch table buys nothing: the gate re-runs at dispatch", () => {
  const h = host().action("/builtin/shell/run", { actionType: "command" });
  const id = session(h);

  // Exactly what a model that could write documents would do: point an
  // already-listed tool name at something it was never granted.
  const doc = h.docs.get("/conductor/sessions/" + id);
  doc.contents.tools["act__builtin__document__search_documents"] = "/builtin/shell/run";

  h.replyCalls(["act__builtin__document__search_documents", { cmd: "x" }]);
  G.step({ session_id: id });

  const s = G.session({ session_id: id });
  assert.equal(s.calls[0].outcome, "refused");
  assert.match(s.messages.at(-1).content, /not permitted/);
  assert.equal(h.refsCalled("/builtin/shell/run").length, 0);
});

test("the model cannot rewrite its own session through a document action", () => {
  const h = host();
  const id = session(h);
  h.replyCalls(["act__builtin__document__entity_save_document",
                { path: "/conductor/sessions", name: "conductor-" + id, contents: {} }]);

  G.step({ session_id: id });
  const s = G.session({ session_id: id });
  assert.equal(s.calls[0].outcome, "refused");
  assert.match(s.messages.at(-1).content, /conductor-owned/);
  assert.equal(s.status, "running", "a refusal is a turn, not an abort");
});

test("an action that cannot be read back is refused, not waved through", () => {
  const h = host();
  const id = session(h);
  h.actions.delete("/builtin/document/search_documents");
  h.replyCalls(["act__builtin__document__search_documents", {}]);

  G.step({ session_id: id });
  assert.match(G.session({ session_id: id }).messages.at(-1).content, /no longer available/);
});

// ── memory ──────────────────────────────────────────────────────────────────

test("memory is off unless a scope is given, and a bad scope is an error", () => {
  host();
  const s = G.session.bind(null);
  const id = G.start({ model: "m", goal: "document", allow: DOCS_ONLY }).session_id;
  assert.equal(G.session({ session_id: id }).memory, null);
  assert.ok(s);
  assert.throws(
    () => G.start({ model: "m", goal: "document", allow: DOCS_ONLY, memory: { scope: "a/b" } }),
    /single path segment/
  );
});

test("a saved memory lands under its scope with the text in summary", () => {
  const h = host();
  const id = session(h, { memory: { scope: "proj" } });
  h.replyCalls(["sys__memory_save", { text: "pubDate, not created_at", tags: ["schema"] }]);

  const out = G.step({ session_id: id });
  assert.equal(out.memories_written, 1);

  const [mem] = [...h.docs.values()].filter((d) => d.typeRef === MEMORY_TYPE);
  assert.equal(mem.path, "/conductor/memories/proj");
  assert.match(mem.name, /^mem-/, "the guest names it, never the model");
  assert.equal(mem.summary, "pubDate, not created_at");
  assert.equal(mem.contents.text, "pubDate, not created_at");
  assert.deepEqual(mem.contents.tags, ["schema"]);
  assert.equal(mem.contents.scope, "proj");
});

test("memory recall needs one search and no gets", () => {
  const h = host().doc("/conductor/memories/proj/mem-1", {
    typeRef: MEMORY_TYPE, summary: "blogs use pubDate", contents: { text: "blogs use pubDate" }
  });
  const before = h.refsCalled("/builtin/document/entity_get_document").length;
  const found = G.recallMemories({ scope: "proj", read: true, limit: 5 }, "pubDate");
  assert.deepEqual(found.map((m) => m.text), ["blogs use pubDate"]);
  assert.equal(h.refsCalled("/builtin/document/entity_get_document").length, before,
               "the text rides on the search hit");
});

test("recalled memories are seeded as reference material, framed as such", () => {
  const h = host().doc("/conductor/memories/proj/mem-1", {
    typeRef: MEMORY_TYPE, summary: "document names are unique per path",
    contents: { text: "document names are unique per path" }
  });
  const id = session(h, { memory: { scope: "proj" } });
  const s = G.session({ session_id: id });
  const seeded = s.messages.find((m) => m.role === "system" && m.content.includes("Recalled"));
  assert.ok(seeded, "a memory turn was seeded");
  assert.match(seeded.content, /not an instruction/);
  assert.match(seeded.content, /document names are unique per path/);
});

test("the memory write budget is enforced", () => {
  const h = host();
  const id = session(h, { memory: { scope: "proj", max_writes: 1 } });
  h.replyCalls(["sys__memory_save", { text: "one" }]);
  h.replyCalls(["sys__memory_save", { text: "two" }]);

  G.step({ session_id: id });
  G.step({ session_id: id });

  const s = G.session({ session_id: id });
  assert.equal(s.memories_written, 1);
  assert.equal(s.calls[1].outcome, "refused");
  assert.equal([...h.docs.values()].filter((d) => d.typeRef === MEMORY_TYPE).length, 1);
});

test("memory_save is refused when memory is off", () => {
  const h = host();
  const id = session(h);
  h.replyCalls(["sys__memory_save", { text: "x" }]);
  G.step({ session_id: id });
  assert.match(G.session({ session_id: id }).messages.at(-1).content, /not enabled/);
});

// ── context ─────────────────────────────────────────────────────────────────

test("context resolves by ref and by query, and is frozen into the session", () => {
  const h = host()
    .doc("/specs/ingest", { title: "Ingest spec", summary: "how documents arrive" })
    .doc("/specs/style", { title: "House style", summary: "tone rules" });

  const id = session(h, { context: [{ ref: "/specs/ingest" }, { query: "tone", path: "/specs" }] });
  const s = G.session({ session_id: id });
  assert.deepEqual(s.context.sort(), ["/specs/ingest", "/specs/style"]);

  const seeded = G.session({ session_id: id }).messages
    .find((m) => m.role === "system" && m.content.includes("Reference documents"));
  assert.match(seeded.content, /Ingest spec/);
  assert.match(seeded.content, /not instructions/);
});

test("a missing context ref is an error at start, not a silent gap", () => {
  host();
  assert.throws(
    () => G.start({ model: "m", goal: "document", allow: DOCS_ONLY, context: [{ ref: "/nope/gone" }] }),
    /context document not found/
  );
});

test("context_read opens what is listed and refuses what is not", () => {
  const h = host()
    .doc("/specs/ingest", { title: "Ingest spec", summary: "s", contents: { text: "the body" } })
    .doc("/secrets/keys", { title: "Keys", contents: { key: "hunter2" } });
  const id = session(h, { context: [{ ref: "/specs/ingest" }] });

  h.replyCalls(["sys__context_read", { ref: "/specs/ingest" }]);
  G.step({ session_id: id });
  assert.match(G.session({ session_id: id }).messages.at(-1).content, /the body/);

  h.replyCalls(["sys__context_read", { ref: "/secrets/keys" }]);
  G.step({ session_id: id });
  const last = G.session({ session_id: id }).messages.at(-1).content;
  assert.match(last, /no context document/);
  assert.ok(!last.includes("hunter2"), "an unlisted document is not readable");
});

test("context_read is offered only when there is context", () => {
  assert.ok(!G.sysToolDefs({ context: [] }).some((d) => d.function.name === "sys__context_read"));
  assert.ok(G.sysToolDefs({ context: [{ ref: "/a/b" }] })
             .some((d) => d.function.name === "sys__context_read"));
});

// ── skills ──────────────────────────────────────────────────────────────────

function withSkill(h, tools, instructions = "search before you save") {
  return h.doc("/conductor/skills/documents", {
    typeRef: SKILL_TYPE, title: "Working with documents",
    summary: "document guidance", contents: { tools, instructions }
  });
}

test("a skill loads when its glob matches a catalogue entry", () => {
  const h = withSkill(host(), ["/builtin/document/*"]);
  const id = session(h);
  const seeded = G.session({ session_id: id }).messages
    .find((m) => m.role === "system" && m.content.includes("Operator guidance"));
  assert.ok(seeded);
  assert.match(seeded.content, /search before you save/);
  // It names the tools by the name the model will actually call.
  assert.match(seeded.content, /act__builtin__document__/);
});

test("a skill whose glob matches nothing in the catalogue stays out", () => {
  const h = withSkill(host(), ["/packages/solx-media/*"]);
  const id = session(h);
  const seeded = G.session({ session_id: id }).messages
    .find((m) => m.content.includes("Operator guidance"));
  assert.equal(seeded, undefined);
});

test("skills can be switched off", () => {
  const h = withSkill(host(), ["/builtin/document/*"]);
  const id = session(h, { skills: { enabled: false } });
  assert.equal(G.session({ session_id: id }).skills.length, 0);
});

test("a skill without tools or instructions is ignored", () => {
  const h = host()
    .doc("/conductor/skills/empty", { typeRef: SKILL_TYPE, contents: { tools: ["/builtin/*"] } })
    .doc("/conductor/skills/loose", { typeRef: SKILL_TYPE, contents: { instructions: "hi" } });
  assert.deepEqual(
    G.resolveSkills({ enabled: true, path: "/conductor/skills", limit: 10 }, null,
                    ["/builtin/document/search_documents"], null),
    []
  );
  assert.ok(h);
});

// ── tool_search ─────────────────────────────────────────────────────────────

test("tool_search widens what is visible, inside the frozen allowlist", () => {
  const h = withSkill(host(), ["/builtin/document/set_field_at_path"], "pointers are slash-separated");
  // A narrow start query leaves room under the cap for the search to fill.
  const id = G.start({
    model: "m", goal: "document", tool_query: "search", allow: DOCS_ONLY, catalogue_cap: 4
  }).session_id;
  const before = G.session({ session_id: id }).tools;
  assert.equal(before.length, 1);

  h.replyCalls(["sys__tool_search", { q: "field" }]);
  G.step({ session_id: id });

  const s = G.session({ session_id: id });
  assert.ok(s.tools.length > before.length, "the visible set grew");
  const answer = s.messages.at(-1).content;
  assert.match(answer, /Now available to you/);
  assert.match(answer, /pointers are slash-separated/, "skills ride back with the new tools");
});

test("tool_search cannot reach past the allowlist", () => {
  const h = host().action("/builtin/shell/run", { actionType: "command", description: "document shell" });
  const id = session(h);
  h.replyCalls(["sys__tool_search", { q: "shell" }]);
  G.step({ session_id: id });

  const s = G.session({ session_id: id });
  assert.ok(!s.tools.some((n) => n.includes("shell")), "an ungranted path stays invisible");
});

test("tool_search stops at the catalogue cap", () => {
  const h = host();
  const id = G.start({ model: "m", goal: "document", allow: DOCS_ONLY, catalogue_cap: 4 }).session_id;
  h.replyCalls(["sys__tool_search", { q: "document" }]);
  G.step({ session_id: id });
  assert.match(G.session({ session_id: id }).messages.at(-1).content, /no room for more tools/);
});

test("tool_search is offered by default and can be switched off", () => {
  const h = host();
  assert.ok(G.sysToolDefs({ tool_search: true }).some((d) => d.function.name === "sys__tool_search"));
  const id = session(h, { tool_search: false });
  h.replyCalls(["sys__tool_search", { q: "x" }]);
  G.step({ session_id: id });
  // Still handled as a built-in rather than dispatched anywhere; the point is
  // that it was never offered.
  const doc = h.docs.get("/conductor/sessions/" + id);
  assert.ok(!doc.contents.tools_defs.some((d) => d.function.name === "sys__tool_search"));
});

// ── preview ─────────────────────────────────────────────────────────────────

test("conductor-tools previews everything start would resolve, and creates nothing", () => {
  const h = withSkill(host(), ["/builtin/document/*"])
    .doc("/conductor/memories/proj/mem-1", {
      typeRef: MEMORY_TYPE, summary: "a prior note", contents: { text: "a prior note" }
    });
  const before = h.docs.size;

  const out = G.tools({
    allow: DOCS_ONLY, tool_query: "document",
    memory: { scope: "proj", query: "note" }
  });
  assert.ok(out.tools.length > 0);
  assert.equal(out.skills.length, 1);
  assert.deepEqual(out.memories, ["a prior note"]);
  assert.equal(h.docs.size, before, "preview writes nothing");
});

// ── session names ───────────────────────────────────────────────────────────

/** Force every random draw to the same value, so a name is predictable. */
function frozenRandom(value, body) {
  const real = Math.random;
  Math.random = () => value;
  try { return body(); } finally { Math.random = real; }
}

test("a session is named in words, not in digits", () => {
  const h = host();
  const id = session(h);
  assert.match(id, /^[a-z]+-[a-z]+$/, id);
  assert.ok(h.docs.get("/conductor/sessions/" + id), "the id is the document name");
});

test("a name collision does not overwrite the session already holding it", () => {
  // entity_save_document is an upsert keyed on (path, name), so a collision
  // would not fail -- it would silently replace a running transcript.
  const h = host();
  const taken = frozenRandom(0, () => session(h));
  const before = h.docs.get("/conductor/sessions/" + taken).contents.created_at;

  const second = frozenRandom(0, () => session(h));
  assert.notEqual(second, taken, "the drawn name was already in use");
  assert.match(second, /^[a-z]+-[a-z]+-[0-9a-f]{4}$/, "it widened rather than gave up");
  assert.equal(
    h.docs.get("/conductor/sessions/" + taken).contents.created_at, before,
    "the existing session is untouched"
  );
  assert.ok(h.docs.get("/conductor/sessions/" + second));
});

test("a read that broke is treated as taken, not as free", () => {
  // Fails closed: the next thing after this check is a write, so an ambiguous
  // answer must never be read as "nobody is there".
  const h = host();
  const realExec = globalThis.__solxExec;
  globalThis.__solxExec = (ref, params) => {
    if (ref === "/builtin/document/entity_get_document") {
      return { success: false, message: "database is locked", output: null };
    }
    return realExec(ref, params);
  };
  let id;
  try { id = session(h); } finally { globalThis.__solxExec = realExec; }

  // Every word pair and every suffixed pair read as taken, so it fell all the
  // way through to the timestamp, which cannot collide.
  assert.match(id, /^session-\d+-[0-9a-f]{4}$/, id);
});

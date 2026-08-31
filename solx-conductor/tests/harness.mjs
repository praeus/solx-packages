// A fake solx host, so the guest can be exercised without a solx install.
//
// The guest reaches everything through one import — `exec(ref, jsonParams)` —
// which makes it cheap to stand up: an in-memory action registry, an in-memory
// document store, and a scripted model. What this cannot check is whether the
// *real* builtins behave as modelled, so every fake below mirrors a behaviour
// that was read out of solx-core rather than assumed:
//
//   - `entity_save_document` requires `typeRef` on create (solx-docs::save)
//   - `search_documents` hits carry no contents, only {path,name,title,summary}
//   - `search_actions` takes camelCase `pathPrefix`/`excludeHidden`
//   - `entity_get_action` reports a hidden action as not-found
//   - a missing document reports `not found: ...`, which the guest matches on
//
// If solx-core changes one of those, these tests keep passing and the package
// breaks — which is the honest limit of a fake host, and why the .solx checks
// in the README still matter.

import { readFile, writeFile, mkdir, copyFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

const SRC = new URL("../src/solx-conductor.js", import.meta.url);

// Names the guest defines that tests want to reach. The guest exports only
// `runner`, so the loader appends these.
const INTERNALS = [
  "globMatches", "permitted", "normalizeAllow", "reservedDocWrite", "validSegment",
  "isAtOrUnder", "encodeToolName", "normalizeSchema", "resolveCatalogue",
  "resolveSkills", "resolveContext", "recallMemories", "sysToolDefs", "isSysTool",
  "gateCall", "start", "step", "run", "session", "tools"
];

/** Build the module under test, with the WIT imports swapped for a fake. */
export async function load() {
  const dir = path.join(tmpdir(), "solx-conductor-tests");
  await mkdir(dir, { recursive: true });
  const file = path.join(dir, "guest-" + process.pid + ".mjs");

  let src = await readFile(SRC, "utf8");
  src = src
    .replace(/^import \{ exec \} from "sol:actions\/action-exec@0\.1\.0";$/m,
             "const exec = (ref, params) => globalThis.__solxExec(ref, params);")
    .replace(/^import \{ log \} from "sol:actions\/logger@0\.1\.0";$/m,
             "const log = () => {};");
  if (src.includes("sol:actions")) throw new Error("an import was not rewritten");
  // ./names.js is an ordinary relative import that node resolves the same way
  // componentize-qjs does, so it only has to be copied next to the rewritten
  // entry module.
  await copyFile(new URL("../src/names.js", import.meta.url), path.join(dir, "names.js"));
  src += "\nexport const __internals = { " + INTERNALS.join(", ") + " };\n";


  await writeFile(file, src, "utf8");
  return import("file://" + file.replace(/\\/g, "/"));
}

const ok = (value) => ({ success: true, message: null, output: JSON.stringify(value) });
const fail = (message) => ({ success: false, message, output: null });

function underPrefix(p, prefix) {
  if (!prefix || prefix === "/") return true;
  return p === prefix || p.startsWith(prefix + "/");
}

// Very rough stand-in for FTS5: every whitespace term must appear somewhere in
// the indexed text. Real ranking differs; term-AND does not.
function matches(q, text) {
  if (!q) return true;
  return String(q).toLowerCase().split(/\s+/).filter(Boolean)
    .every((t) => text.toLowerCase().includes(t));
}

export class FakeHost {
  constructor() {
    this.actions = new Map();   // ref -> action row
    this.docs = new Map();      // ref -> document
    this.types = new Map();     // ref -> {schema}
    this.calls = [];            // every exec, in order
    this.replies = [];          // scripted ollama-chat messages
    this.cancelled = false;
    this.install();
  }

  install() { globalThis.__solxExec = (ref, params) => this.exec(ref, params); }

  action(ref, row = {}) {
    const i = ref.lastIndexOf("/");
    this.actions.set(ref, {
      path: ref.slice(0, i) || "/", name: ref.slice(i + 1),
      actionType: "script", capabilities: [], description: "", caption: "", ...row
    });
    return this;
  }

  doc(ref, doc = {}) {
    const i = ref.lastIndexOf("/");
    this.docs.set(ref, {
      path: ref.slice(0, i) || "/", name: ref.slice(i + 1),
      title: "", summary: "", typeRef: "/types/core/Object", contents: {}, ...doc
    });
    return this;
  }

  /** Queue one assistant message for the next ollama-chat call. */
  reply(message) { this.replies.push(message); return this; }

  /** Queue an assistant turn asking for tool calls. */
  replyCalls(...calls) {
    return this.reply({ role: "assistant", content: "", tool_calls: calls.map(
      ([name, args]) => ({ function: { name, arguments: args } })) });
  }

  refsCalled(ref) { return this.calls.filter((c) => c.ref === ref); }

  exec(ref, raw) {
    const p = JSON.parse(raw || "{}");
    this.calls.push({ ref, params: p });

    switch (ref) {
      case "/builtin/console/print": return ok({});
      case "/builtin/action/cancelled": return ok({ cancelled: this.cancelled });

      case "/builtin/action/search_actions": {
        // camelCase, exactly as ActionSearchQuery deserializes it. A guest
        // sending pathPrefix as path_prefix would silently scan everything;
        // this fake reproduces that by simply not seeing the wrong key.
        const items = [...this.actions.values()]
          .filter((a) => underPrefix(a.path, p.pathPrefix))
          .filter((a) => !(p.excludeHidden && a.capabilities.includes("solx:hidden")))
          .filter((a) => matches(p.q, [a.path, a.name, a.caption, a.description].join(" ")))
          .slice(0, p.limit ?? 50);
        return ok({ items, total: items.length, limit: p.limit ?? 50, offset: 0 });
      }

      case "/builtin/action/entity_get_action": {
        const a = this.actions.get((p.path === "/" ? "" : p.path) + "/" + p.name);
        if (!a) return fail("action not found");
        if (p.excludeHidden && a.capabilities.includes("solx:hidden")) {
          return fail("action not found: " + p.path + "/" + p.name);
        }
        return ok(a);
      }

      case "/builtin/type/entity_get_type": {
        const t = this.types.get((p.path === "/" ? "" : p.path) + "/" + p.name);
        return t ? ok(t) : fail("type not found");
      }

      case "/builtin/document/entity_get_document": {
        const d = this.docs.get((p.path === "/" ? "" : p.path) + "/" + p.name);
        return d ? ok(d) : fail("not found: document " + p.path + "/" + p.name);
      }

      case "/builtin/document/entity_save_document": {
        const key = (p.path === "/" ? "" : p.path) + "/" + p.name;
        const existing = this.docs.get(key);
        // solx-docs::save — typeRef is required on create, inherited on update.
        const typeRef = p.typeRef ?? existing?.typeRef;
        if (!typeRef) return fail("a type_ref is required to create a document");
        this.docs.set(key, {
          path: p.path, name: p.name,
          title: p.title ?? existing?.title ?? "",
          summary: p.summary ?? existing?.summary ?? "",
          typeRef,
          contents: p.contents ?? existing?.contents ?? {}
        });
        return ok(this.docs.get(key));
      }

      case "/builtin/document/search_documents": {
        // Hits are shallow: no contents. That is what makes the memory design
        // (text in `summary`) cheap and the skill design (a get per hit) not.
        const hits = [...this.docs.values()]
          .filter((d) => underPrefix(d.path, p.pathPrefix))
          .filter((d) => !p.typeRef || d.typeRef === p.typeRef)
          .filter((d) => matches(p.q, [d.name, d.title, d.summary,
                                        JSON.stringify(d.contents)].join(" ")))
          .slice(0, p.limit ?? 20)
          .map((d) => ({ id: d.name, path: d.path, name: d.name, title: d.title,
                         summary: d.summary, typeRef: d.typeRef, score: 1 }));
        return ok({ hits, total: hits.length, limit: p.limit ?? 20, offset: 0 });
      }

      case "/packages/solx-ollama/ollama-chat": {
        if (this.replies.length === 0) return fail("no scripted reply left");
        return ok({ message: this.replies.shift() });
      }
    }

    // Anything else is a dispatched tool. Succeeds unless the row says
    // otherwise, so tests can assert on what reached this point.
    const a = this.actions.get(ref);
    if (!a) return fail("no such action " + ref);
    if (a.failWith) return fail(a.failWith);
    return ok({ ran: ref, params: p });
  }
}

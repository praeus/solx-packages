/**
 * A fake solx host, so the harness can be exercised without a solx install.
 *
 * Ported from `solx-conductor/tests/harness.mjs`. That version had to rewrite
 * the guest's WIT imports to reach a fake; this one does not need to, because
 * the harness already talks to exactly one injectable seam -- the same
 * `WidgetClient` shape a real host hands the widget. So the fake *is* a
 * client, and nothing under test knows the difference.
 *
 * What this cannot check is whether the *real* builtins behave as modelled,
 * so every fake below mirrors a behaviour that was read out of solx-core
 * rather than assumed:
 *
 *   - `entity_save_document` requires `type_ref` on create, **snake_case** --
 *     `DocumentInput` (solx-surface/src/entities.rs) has no camelCase rename,
 *     unlike the search queries. The conductor's version of this fake
 *     asserted `typeRef`, so it agreed with a real bug and hid it; the live
 *     test is what caught it.
 *   - `search_documents` hits carry no contents, only {path,name,title,summary}
 *   - `search_actions` takes camelCase `pathPrefix`/`excludeHidden`
 *   - `entity_get_action` reports a hidden action as not-found
 *   - a missing document reports `not found: ...`, which `sessionNameTaken`
 *     matches on
 *   - action params are schema-validated, so an explicit `null` for an
 *     optional string is an **error**, not "absent". `{q: query || null}`
 *     therefore breaks every no-query search. The live test caught this too;
 *     `rejectNulls` below reproduces it so the fake cannot hide it again.
 *
 * If solx-core changes one of those, these tests keep passing and the package
 * breaks -- the honest limit of a fake host, and why the live checks in the
 * README still matter.
 */

import { hostFromClient, type ExecClient, type Host } from "../src/harness/host";

type Json = Record<string, unknown>;

export interface ActionRow {
  path: string;
  name: string;
  actionType: string;
  capabilities: string[];
  description: string;
  caption: string;
  paramTypeRef?: string;
  failWith?: string;
}

export interface DocRow {
  path: string;
  name: string;
  title: string;
  summary: string;
  typeRef: string;
  contents: Json;
}

const ok = (value: unknown) => ({ success: true, message: null, result: value });
const fail = (message: string) => ({ success: false, message, result: null });

function underPrefix(p: string, prefix?: string | null): boolean {
  if (!prefix || prefix === "/") return true;
  return p === prefix || p.startsWith(prefix + "/");
}

/**
 * Very rough stand-in for FTS5: every whitespace term must appear somewhere in
 * the indexed text. Real ranking differs; term-AND does not.
 */
function matches(q: unknown, text: string): boolean {
  if (!q) return true;
  return String(q)
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((t) => text.toLowerCase().includes(t));
}

export class FakeHost implements ExecClient {
  actions_: Map<string, ActionRow> = new Map();
  docs: Map<string, DocRow> = new Map();
  types: Map<string, { schema: unknown }> = new Map();
  /** Every exec, in order. */
  calls: { ref: string; params: Json }[] = [];
  /** Scripted ollama-chat messages. */
  replies: Json[] = [];

  action(ref: string, row: Partial<ActionRow> = {}): this {
    const i = ref.lastIndexOf("/");
    this.actions_.set(ref, {
      path: ref.slice(0, i) || "/",
      name: ref.slice(i + 1),
      actionType: "script",
      capabilities: [],
      description: "",
      caption: "",
      ...row,
    });
    return this;
  }

  doc(ref: string, doc: Partial<DocRow> = {}): this {
    const i = ref.lastIndexOf("/");
    this.docs.set(ref, {
      path: ref.slice(0, i) || "/",
      name: ref.slice(i + 1),
      title: "",
      summary: "",
      typeRef: "/types/core/Object",
      contents: {},
      ...doc,
    });
    return this;
  }

  type(ref: string, schema: unknown): this {
    this.types.set(ref, { schema });
    return this;
  }

  /** Queue one assistant message for the next ollama-chat call. */
  reply(message: Json): this {
    this.replies.push(message);
    return this;
  }

  /** Queue an assistant turn asking for tool calls. */
  replyCalls(...calls: [string, Json][]): this {
    return this.reply({
      role: "assistant",
      content: "",
      tool_calls: calls.map(([name, args]) => ({ function: { name, arguments: args } })),
    });
  }

  /** Queue an assistant turn that hands the turn back. */
  replyText(content: string): this {
    return this.reply({ role: "assistant", content });
  }

  refsCalled(ref: string): { ref: string; params: Json }[] {
    return this.calls.filter((c) => c.ref === ref);
  }

  callNames(): string[] {
    return this.calls.map((c) => c.ref);
  }

  /** The session document the harness has been writing, if any. */
  session(id: string): Json | undefined {
    return this.docs.get("/agent/sessions/" + id)?.contents;
  }

  readonly actions = {
    exec: async (path: string, name: string, params?: unknown) => {
      return this.exec((path === "/" ? "" : path) + "/" + name, (params ?? {}) as Json);
    },
  };

  exec(ref: string, p: Json): { success: boolean; message: string | null; result: unknown } {
    this.calls.push({ ref, params: p });

    // solx validates params against the action's JSON Schema before running
    // it, and an optional string field is `{"type":"string"}` -- so an
    // explicit null is rejected outright rather than read as absent.
    for (const key of ["q", "pathPrefix", "typeRef", "type_ref", "path", "name"]) {
      if (key in p && p[key] === null) {
        return fail("null is not of type " + JSON.stringify("string") + " at /" + key);
      }
    }

    switch (ref) {
      case "/builtin/console/print":
        return ok({});

      case "/builtin/action/search_actions": {
        // camelCase, exactly as ActionSearchQuery deserializes it. A caller
        // sending pathPrefix as path_prefix would silently scan everything;
        // this fake reproduces that by simply not seeing the wrong key.
        const items = [...this.actions_.values()]
          .filter((a) => underPrefix(a.path, p.pathPrefix as string))
          .filter((a) => !(p.excludeHidden && a.capabilities.includes("solx:hidden")))
          .filter((a) => matches(p.q, [a.path, a.name, a.caption, a.description].join(" ")))
          .slice(0, (p.limit as number) ?? 50);
        return ok({ items, total: items.length, limit: (p.limit as number) ?? 50, offset: 0 });
      }

      case "/builtin/action/entity_get_action": {
        const a = this.actions_.get((p.path === "/" ? "" : p.path) + "/" + p.name);
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
        // solx-docs::save -- type_ref is required on create, inherited on
        // update. Deliberately does NOT accept `typeRef`: a fake that is more
        // forgiving than the real thing is worse than no fake.
        const typeRef = (p.type_ref as string) ?? existing?.typeRef;
        if (!typeRef) return fail("a type_ref is required to create a document");
        this.docs.set(key, {
          path: p.path as string,
          name: p.name as string,
          title: (p.title as string) ?? existing?.title ?? "",
          summary: (p.summary as string) ?? existing?.summary ?? "",
          typeRef,
          // Deep-copied on write, like a real store: the harness mutates its
          // session object in place, so a stored reference would let later
          // mutations retroactively change what was "saved".
          contents: JSON.parse(
            JSON.stringify(p.contents ?? existing?.contents ?? {}),
          ) as Json,
        });
        return ok(this.docs.get(key));
      }

      case "/builtin/document/search_documents": {
        // Hits are shallow: no contents. That is what makes the memory design
        // (text in `summary`) cheap and the skill design (a get per hit) not.
        const hits = [...this.docs.values()]
          .filter((d) => underPrefix(d.path, p.pathPrefix as string))
          .filter((d) => !p.typeRef || d.typeRef === p.typeRef)
          .filter((d) =>
            matches(p.q, [d.name, d.title, d.summary, JSON.stringify(d.contents)].join(" ")),
          )
          .slice(0, (p.limit as number) ?? 20)
          .map((d) => ({
            id: d.name,
            path: d.path,
            name: d.name,
            title: d.title,
            summary: d.summary,
            typeRef: d.typeRef,
            score: 1,
          }));
        return ok({ hits, total: hits.length, limit: (p.limit as number) ?? 20, offset: 0 });
      }

      case "/packages/solx-ollama/ollama-chat": {
        if (this.replies.length === 0) return fail("no scripted reply left");
        return ok({ message: this.replies.shift() });
      }
    }

    // Anything else is a dispatched tool. Succeeds unless the row says
    // otherwise, so tests can assert on what reached this point.
    const a = this.actions_.get(ref);
    if (!a) return fail("no such action " + ref);
    if (a.failWith) return fail(a.failWith);
    return ok({ ran: ref, params: p });
  }
}

/** A fake host plus the `Host` the harness takes. */
export function fake(): { fake: FakeHost; host: Host } {
  const f = new FakeHost();
  return { fake: f, host: hostFromClient(f) };
}

/** The grant every test starts from unless it needs something narrower. */
export const DOCS_GRANT = [{ path: "/builtin/document" }];

/** Seed a host with a handful of ordinary, permitted document actions. */
export function withDocActions(f: FakeHost): FakeHost {
  return f
    .action("/builtin/document/search_documents", { description: "search documents" })
    .action("/builtin/document/entity_get_document", { description: "get a document" })
    .action("/builtin/document/entity_save_document", {
      description: "save a document",
      capabilities: ["solx:destructive"],
    });
}

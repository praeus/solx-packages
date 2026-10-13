/**
 * A fake solx host, so the inquiry pipeline can be exercised without a solx
 * install.
 *
 * Ported from `solx-agent/tests/fakeHost.ts`, and deliberately keeps every
 * behaviour that file records as read out of solx-core rather than assumed --
 * the ones that have drawn blood before:
 *
 *   - `entity-save-document` requires camelCase `typeRef` on create
 *     (`DocumentInput` is `#[serde(rename_all = "camelCase")]`).
 *   - `search-documents` returns whole documents (`Page<Document>`, contents
 *     included, no `score`).
 *   - action params are schema-validated, so an explicit `null` for an optional
 *     string is an **error**, not "absent". `rejectNulls` below reproduces it,
 *     because the `q: null` bug is exactly the one this package is most
 *     exposed to: `search`/`list` are the same builtin, and `list` passes no
 *     `q` deliberately.
 *
 * Trimmed to what inquiry uses: no grant, no destructive capabilities, no
 * detached invocation protocol beyond the one `ollama-chat` path, and no
 * `sys__` tools. What is added is `entity-list-document-paths` and
 * `entity-list-types`, which the corpus index reads and the agent never did.
 *
 * One schema distinction this fake deliberately preserves, because it is a
 * trap: `excludeHidden` exists only on `ActionSearchQuery` (which
 * `search-actions` deserializes), not on `ListOptions` (which the list
 * endpoints take). Unknown keys are *dropped in silence*, so the fake ignores
 * them too rather than rejecting -- and a test asserts the corpus index does
 * not send one to a list endpoint, where it would be believed rather than
 * refused.
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

/** Rough stand-in for FTS5: every whitespace term must appear, prefix-matched.
 *  The AND is what `fts_match_query` does, and the whole reason a sentence-shaped
 *  `q` matches nothing -- so the fake must reproduce it, not forgive it. */
function matches(q: unknown, text: string): boolean {
  if (!q) return true;
  const hay = text.toLowerCase();
  return String(q)
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((t) => hay.includes(t));
}

export class FakeHost implements ExecClient {
  actions_: Map<string, ActionRow> = new Map();
  docs: Map<string, DocRow> = new Map();
  types: Map<string, { schema: unknown; description?: string }> = new Map();
  /** Every exec, in order. */
  calls: { ref: string; params: Json }[] = [];
  /** Scripted ollama-chat messages. */
  replies: Json[] = [];
  invocations: Map<string, { status: string; result: unknown; error: string | null }> = new Map();
  /** Parameters of every ollama-chat call, in order -- plan then answer. */
  chatParams: Json[] = [];
  /** Params of the most recent ollama-chat call, for a quick assertion. */
  lastChatParams: Json | null = null;
  private seq = 0;

  action(ref: string, row: Partial<ActionRow> = {}): this {
    const i = ref.lastIndexOf("/");
    this.actions_.set(ref, {
      path: ref.slice(0, i) || "/",
      name: ref.slice(i + 1),
      actionType: "internal",
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

  type(ref: string, schema: unknown, description?: string): this {
    this.types.set(ref, { schema, description });
    return this;
  }

  /** Queue one assistant message for the next ollama-chat call. */
  reply(message: Json): this {
    this.replies.push(message);
    return this;
  }

  /** Queue an assistant turn that carries text (a plan, or an answer). */
  replyText(content: string): this {
    return this.reply({ role: "assistant", content });
  }

  /** Queue a planner reply: a plan object, serialized as `format` output. */
  replyPlan(plan: unknown): this {
    return this.replyText(typeof plan === "string" ? plan : JSON.stringify(plan));
  }

  refsCalled(ref: string): { ref: string; params: Json }[] {
    return this.calls.filter((c) => c.ref === ref);
  }

  callNames(): string[] {
    return this.calls.map((c) => c.ref);
  }

  /** The inquiry document the pipeline has been writing, if any. */
  inquiry(id: string): Json | undefined {
    return this.docs.get("/inquiry/sessions/" + id)?.contents;
  }

  readonly actions = {
    exec: async (path: string, name: string, params?: unknown) => {
      return this.exec((path === "/" ? "" : path) + "/" + name, (params ?? {}) as Json);
    },
  };

  exec(ref: string, p: Json): { success: boolean; message: string | null; result: unknown } {
    this.calls.push({ ref, params: p });

    // solx validates params against the schema before running, and an optional
    // string field is `{"type":"string"}` -- an explicit null is rejected.
    for (const key of ["q", "pathPrefix", "typeRef", "path", "name", "limit"]) {
      if (key in p && p[key] === null) {
        return fail("null is not of type string at /" + key);
      }
    }

    switch (ref) {
      case "/builtin/document/entity-get-document": {
        const d = this.docs.get((p.path === "/" ? "" : p.path) + "/" + p.name);
        return d ? ok(d) : fail("not found: document " + p.path + "/" + p.name);
      }

      case "/builtin/document/entity-save-document": {
        const key = (p.path === "/" ? "" : p.path) + "/" + p.name;
        const existing = this.docs.get(key);
        const typeRef = (p.typeRef as string) ?? existing?.typeRef;
        if (!typeRef) return fail("a type_ref is required to create a document");
        this.docs.set(key, {
          path: p.path as string,
          name: p.name as string,
          title: (p.title as string) ?? existing?.title ?? "",
          summary: (p.summary as string) ?? existing?.summary ?? "",
          typeRef,
          contents: JSON.parse(JSON.stringify(p.contents ?? existing?.contents ?? {})) as Json,
        });
        return ok(this.docs.get(key));
      }

      case "/builtin/document/search-documents": {
        const items = [...this.docs.values()]
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
            contents: d.contents,
          }));
        return ok({ items, total: items.length, limit: (p.limit as number) ?? 20, offset: 0 });
      }

      case "/builtin/document/entity-list-documents": {
        const items = [...this.docs.values()]
          .filter((d) => underPrefix(d.path, p.pathPrefix as string))
          .slice(0, (p.limit as number) ?? 50)
          .map((d) => ({ path: d.path, name: d.name, title: d.title, typeRef: d.typeRef }));
        return ok({ items, total: items.length });
      }

      case "/builtin/document/entity-list-document-paths": {
        const counts: Record<string, number> = {};
        for (const d of this.docs.values()) {
          if (!underPrefix(d.path, p.pathPrefix as string)) continue;
          counts[d.path] = (counts[d.path] || 0) + 1;
        }
        const items = Object.keys(counts)
          .map((path) => ({ path, count: counts[path] }))
          .slice(0, (p.limit as number) ?? 50);
        return ok({ items, total: items.length });
      }

      case "/builtin/type/entity-list-types": {
        const items = [...this.types.entries()]
          .map(([ref, t]) => {
            const i = ref.lastIndexOf("/");
            return {
              path: ref.slice(0, i) || "/",
              name: ref.slice(i + 1),
              title: ref.slice(i + 1),
              schema: t.schema,
              description: t.description ?? "",
            };
          })
          .filter((t) => underPrefix(t.path, p.pathPrefix as string))
          .slice(0, (p.limit as number) ?? 50);
        return ok({ items, total: items.length });
      }

      case "/builtin/action/search-actions": {
        const items = [...this.actions_.values()]
          .filter((a) => underPrefix(a.path, p.pathPrefix as string))
          .filter((a) => matches(p.q, [a.path, a.name, a.caption, a.description].join(" ")))
          .slice(0, (p.limit as number) ?? 50);
        return ok({ items, total: items.length, limit: (p.limit as number) ?? 50, offset: 0 });
      }

      case "/packages/solx-ollama/ollama-list-models": {
        return ok({ models: [{ name: "qwen3:4b", capabilities: ["tools"] }] });
      }

      case "/packages/solx-ollama/ollama-chat": {
        this.lastChatParams = p;
        this.chatParams.push(p);
        if (this.replies.length === 0) return fail("no scripted reply left");
        return ok({ message: this.replies.shift() });
      }

      case "/builtin/action/start": {
        const target = (p.path === "/" ? "" : p.path) + "/" + p.name;
        if (target !== "/packages/solx-ollama/ollama-chat") {
          return fail("start not modelled for " + target);
        }
        if (this.replies.length === 0) return fail("no scripted reply left");
        const id = "inv-" + ++this.seq;
        const chatParams = (p.params ?? {}) as Json;
        this.lastChatParams = chatParams;
        this.chatParams.push(chatParams);
        this.invocations.set(id, {
          status: "ok",
          result: { message: this.replies.shift() },
          error: null,
        });
        return ok({ invocation_id: id, action_ref: target, console_seq_start: 0 });
      }

      case "/builtin/action/poll": {
        const inv = this.invocations.get(p.invocation_id as string);
        if (!inv) return fail("no such invocation " + p.invocation_id);
        return ok({
          invocation_id: p.invocation_id,
          status: inv.status,
          result: inv.result,
          error: inv.error,
        });
      }

      case "/builtin/action/stop": {
        const inv = this.invocations.get(p.invocation_id as string);
        if (!inv) return fail("no such invocation " + p.invocation_id);
        inv.status = "cancelled";
        return ok({ invocation_id: p.invocation_id, status: "cancelled", result: null, error: null });
      }
    }

    const a = this.actions_.get(ref);
    if (!a) return fail("no such action " + ref);
    if (a.failWith) return fail(a.failWith);
    return ok({ ran: ref, params: p });
  }
}

/** A fake host plus the `Host` the pipeline takes. */
export function fake(): { fake: FakeHost; host: Host } {
  const f = new FakeHost();
  return { fake: f, host: hostFromClient(f) };
}

/** Seed the read-only builtins the verbs resolve to, so a plan can dispatch. */
export function withInquiryActions(f: FakeHost): FakeHost {
  return f
    .action("/builtin/document/search-documents", { description: "search documents" })
    .action("/builtin/document/entity-list-documents", { description: "list documents" })
    .action("/builtin/document/entity-get-document", { description: "get a document" })
    .action("/builtin/type/entity-list-types", { description: "list types" })
    .action("/builtin/action/search-actions", { description: "search actions" });
}

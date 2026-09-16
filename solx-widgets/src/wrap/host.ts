/**
 * A convenience wrapper around a widget's injected `client.actions.exec`,
 * for widgets whose logic issues more than a handful of ad-hoc calls (an
 * agent loop driving a whole action catalogue, a workflow widget chaining
 * several actions together, etc.). A widget that only needs one or two
 * one-off calls can just use `useSolxWidgetClient()` and
 * `client.actions.exec(path, name, params)` directly — this exists to save
 * re-deriving the same three things every time a widget's calls multiply:
 *
 *   - a single `"path/name"` ref string instead of two parameters,
 *   - a throwing `call()` for calls where failure means the surrounding work
 *     cannot continue, and a non-throwing `try()` for calls whose failure is
 *     meant to be shown or recovered from rather than aborting,
 *   - `compact()`, because solx validates params against a JSON Schema and an
 *     optional field sent as an explicit `null` fails validation outright —
 *     `{q: query || null}` looks harmless and breaks every no-query call.
 *
 * Ported out of `solx-agent`'s harness (originally the seam a wasm guest
 * reached its host through, before that harness moved into the widget) once
 * a second widget needed the same three things — see
 * `solx-packages/docs/widget-system.md`.
 *
 * `ExecClient` is intentionally narrower than `WidgetClient` in
 * `../shared/widgetClient.ts` — only the one method this wrapper actually
 * calls — so a test fixture can satisfy it without also faking
 * `invocations`. A real `WidgetClient` satisfies it structurally with no
 * cast needed.
 */
export interface ExecClient {
  actions: {
    exec(
      path: string,
      name: string,
      params?: unknown,
    ): Promise<{ result?: unknown; success?: boolean; message?: string | null }>;
  };
}

export type Attempt<T> = { ok: true; value: T } | { ok: false; error: string };

/**
 * Drop null/undefined entries from a params object.
 *
 * Not cosmetic. solx validates action params against their JSON Schema, and
 * an optional string field is `{"type":"string"}` — absent is fine, but an
 * explicit `null` fails with `null is not of type "string" at /q` and the
 * whole action errors. Build search/optional params through this.
 */
export function compact(params: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(params)) {
    if (v !== null && v !== undefined) out[k] = v;
  }
  return out;
}

export interface Host {
  /** Throws on failure. For calls where a failure means the surrounding work cannot continue. */
  call<T>(ref: string, params?: unknown): Promise<T>;
  /** Hands the failure back. For calls whose failure the caller is meant to show or recover from. */
  try<T>(ref: string, params?: unknown): Promise<Attempt<T>>;
}

function splitRef(ref: string): { path: string; name: string } {
  const slash = ref.lastIndexOf("/");
  return { path: ref.slice(0, slash) || "/", name: ref.slice(slash + 1) };
}

export function hostFromClient(client: ExecClient): Host {
  return {
    async call<T>(ref: string, params?: unknown): Promise<T> {
      const { path, name } = splitRef(ref);
      const r = await client.actions.exec(path, name, params ?? {});
      if (!r || r.success === false) {
        throw new Error(ref + ": " + ((r && r.message) || "action failed"));
      }
      return r.result as T;
    },

    async try<T>(ref: string, params?: unknown): Promise<Attempt<T>> {
      try {
        const { path, name } = splitRef(ref);
        const r = await client.actions.exec(path, name, params ?? {});
        if (!r || r.success === false) {
          return { ok: false, error: (r && r.message) || "action failed" };
        }
        return { ok: true, value: r.result as T };
      } catch (e) {
        return { ok: false, error: String((e as Error)?.message ?? e) };
      }
    },
  };
}

/**
 * The one seam the harness talks to solx through.
 *
 * The wasm guest this was ported from reached its host through a single
 * import -- `exec(ref, jsonString)` -- and nothing else. That is exactly the
 * shape a widget already has in `WidgetClient.actions.exec(path, name,
 * params)`, which is what made the port mechanical: everything below this
 * file is the guest's logic with `await` in front of the calls.
 *
 * The one real difference is that the guest's `exec` was synchronous and this
 * one is a Promise, so every function below the seam is `async`. Dispatch was
 * already strictly sequential (the conductor ran tool calls in order by
 * design), so no ordering guarantee was lost in the move.
 *
 * Tests substitute a fake host here rather than stubbing `fetch` -- see
 * `tests/fakeHost.ts`.
 */

/** Structurally what `solx-widgets`' `WidgetClient` provides. Not imported:
 *  the harness has no reason to depend on the widget toolkit, and a fake host
 *  is easier to write against a two-method interface. */
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
 * an optional string field is `{"type":"string"}` -- absent is fine, but an
 * explicit `null` fails with `null is not of type "string" at /q` and the
 * whole action errors. So `{q: query || null}` silently breaks every search
 * that has no query, which is exactly the path a catalogue fallback or a
 * path-only context spec takes. Build search params through this.
 */
export function compact(params: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(params)) {
    if (v !== null && v !== undefined) out[k] = v;
  }
  return out;
}

export interface Host {
  /** Throws on failure. For calls where a failure means the turn cannot continue. */
  call<T>(ref: string, params?: unknown): Promise<T>;
  /** Hands the failure back. For anything a model is allowed to see fail. */
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

    // Same contract as `call`, but returns the failure instead of throwing --
    // a tool that fails is a turn the model gets to see and correct from, not
    // an aborted session.
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

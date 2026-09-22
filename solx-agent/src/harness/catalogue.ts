/**
 * Resolving what the model is shown.
 *
 * The action registry *is* a tool catalogue: every row carries a name, a
 * description, and a `paramTypeRef` pointing at a JSON Schema, which is the
 * three things Ollama's tool format wants. So this is selection and gating,
 * not plumbing.
 *
 * Catalogue by search, not by listing: every tool definition is prompt tokens
 * on every iteration, and listing everything a grant permits will drown a 4B
 * model. `tools_dropped` records how many matches fell outside the cap, so
 * truncation is visible in the session rather than inferred from the model
 * behaving oddly.
 */

import { globMatches, isGlob, permitted, refOf, splitRef } from "./gate";
import { GET_TYPE, SEARCH_ACTIONS, SEARCH_FETCH } from "./refs";
import { compact } from "./host";
import type { Host } from "./host";
import type { AllowEntry, ToolDef } from "./types";

interface ActionRow {
  path: string;
  name: string;
  description?: string;
  caption?: string;
  actionType?: string;
  action_type?: string;
  capabilities?: string[];
  paramTypeRef?: string;
  param_type_ref?: string;
}

/**
 * The same readable shape solx-mcp produces, so a name in a transcript looks
 * familiar. Deliberately *not* a port of its decoder: a name is resolved
 * through the session's own map, never parsed back. That is stronger than
 * decoding -- the model cannot synthesize a valid name for an action that was
 * never listed -- and it is why there is no base32 fallback.
 */
export function encodeToolName(path: string, name: string): string {
  const segments = path === "/" ? [] : path.replace(/^\//, "").split("/");
  segments.push(name);
  // Hyphens are a legitimate, common character in action/package names (e.g.
  // "ollama-chat", every renamed builtin) and solx-mcp's own Rust encoder
  // does no sanitization at all -- only characters that would actually
  // break a tool name need stripping here.
  return "act__" + segments.join("__").replace(/[^A-Za-z0-9_-]/g, "_");
}

function flattenNullUnions(node: unknown): void {
  if (!node || typeof node !== "object") return;
  const n = node as Record<string, unknown>;
  if (Array.isArray(n.type)) {
    const real = (n.type as string[]).filter((t) => t !== "null");
    n.type = real.length === 1 ? real[0] : real.length === 0 ? "string" : real;
  }
  if (n.properties && typeof n.properties === "object") {
    for (const key of Object.keys(n.properties as object)) {
      flattenNullUnions((n.properties as Record<string, unknown>)[key]);
    }
  }
  if (n.items) flattenNullUnions(n.items);
}

/**
 * Small local models handle union types poorly: given
 * `{"type":["string","null"]}` they emit the string `"null"`, or omit the
 * field and then apologise. Absence from `required` already carries
 * optionality.
 */
export function normalizeSchema(schema: unknown): Record<string, unknown> {
  if (!schema || typeof schema !== "object") {
    return { type: "object", properties: {} };
  }
  const out = JSON.parse(JSON.stringify(schema)) as Record<string, unknown>;
  flattenNullUnions(out);
  // Ollama wants an object schema even when the type has no properties.
  if (out.type !== "object") return { type: "object", properties: {} };
  if (!out.properties) out.properties = {};
  return out;
}

async function schemaFor(host: Host, action: ActionRow): Promise<Record<string, unknown>> {
  const ref = action.paramTypeRef || action.param_type_ref;
  if (!ref) return { type: "object", properties: {} };
  const parts = splitRef(ref);
  const r = await host.try<{ schema?: unknown }>(GET_TYPE, { path: parts.path, name: parts.name });
  if (!r.ok || !r.value) return { type: "object", properties: {} };
  return normalizeSchema(r.value.schema);
}

export interface Catalogue {
  tools: ToolDef[];
  map: Record<string, string>;
  /** Tool name -> human-readable label (the action's caption, else its name). */
  labels: Record<string, string>;
  dropped: number;
}

/**
 * One catalogue search, with a per-term retry when the phrase matched nothing.
 *
 * `fts_match_query` in solx-docs turns each whitespace-separated term into
 * `"term"*` and **ANDs** them, so a phrase only matches an action whose text
 * contains every word. Measured against a real catalogue: `file` returns 16,
 * `store` 7, `write` 8 -- and `file store write` returns 0. A model driving
 * `sys__tool_search` writes phrases, not keywords, so the AND made tool
 * discovery fail exactly when it was needed most: a session that had run out
 * of catalogue would search, match nothing, and start guessing tool names.
 *
 * The widening lives here rather than in `fts_match_query` because AND is the
 * right default for document search, where precision matters. `resolveSkills`
 * reached the same conclusion from the other side and dropped `q` entirely.
 *
 * Only a *zero* result triggers the retry. A phrase that matched something
 * matched it precisely, and that ranking is better than anything merging can
 * reconstruct. The merge is round-robin by rank, not term-by-term, so each
 * term contributes its best matches before any term contributes its worst --
 * otherwise `cap` would be filled by the first word alone.
 */
async function searchRows(
  host: Host,
  query: string | null,
  pathPrefix: string | null,
): Promise<ActionRow[]> {
  const once = async (q: string | null): Promise<ActionRow[]> => {
    const page = await host.try<{ items?: ActionRow[] }>(
      SEARCH_ACTIONS,
      // `q` is omitted rather than nulled when there is no query -- see
      // `compact`. A null there fails schema validation and the whole search
      // errors, which is how a no-query fallback silently resolves to nothing.
      compact({ q, pathPrefix, limit: SEARCH_FETCH, excludeHidden: true }),
    );
    return page.ok && page.value && Array.isArray(page.value.items) ? page.value.items : [];
  };

  const rows = await once(query);
  if (rows.length > 0 || !query) return rows;

  const terms = query
    .split(/\s+/)
    .map((t) => t.replace(/[^\p{L}\p{N}_-]/gu, ""))
    .filter((t) => t.length > 1);
  if (terms.length < 2) return rows;

  const perTerm: ActionRow[][] = [];
  for (const t of terms) perTerm.push(await once(t));

  const merged: ActionRow[] = [];
  const taken: Record<string, boolean> = {};
  for (let rank = 0; merged.length < SEARCH_FETCH; rank++) {
    let anyLeft = false;
    for (const rowsForTerm of perTerm) {
      if (rank >= rowsForTerm.length) continue;
      anyLeft = true;
      const a = rowsForTerm[rank];
      const ref = refOf(a.path, a.name);
      if (taken[ref]) continue;
      taken[ref] = true;
      merged.push(a);
      if (merged.length >= SEARCH_FETCH) break;
    }
    if (!anyLeft) break;
  }
  return merged;
}

/**
 * Resolve the tools a turn may use: one search per grant prefix, filtered by
 * the gate, capped, and turned into Ollama tool definitions.
 *
 * `excludeHidden: true` is what makes the exclusion list apply -- it is
 * resolved in Rust from config rules unioned with the row's own `solx:hidden`
 * capability, so nothing here has to know the rules. `known` lets a widening
 * search skip what the model already holds.
 */
export async function resolveCatalogue(
  host: Host,
  query: string | null,
  allow: AllowEntry[],
  cap: number,
  known: Record<string, boolean> | null,
): Promise<Catalogue> {
  const seen: Record<string, boolean> = {};
  const tools: ToolDef[] = [];
  const map: Record<string, string> = {};
  const labels: Record<string, string> = {};
  let matched = 0;

  for (const rule of allow) {
    // A pattern grant (`*`, `/packages/*`) cannot be pushed down to
    // `pathPrefix`: that filter is a SQL prefix match, so it would look for a
    // path literally named `/*` and return nothing -- the grant would resolve
    // to an empty catalogue even though the gate would have allowed every row.
    // Search unscoped instead and let `permitted` below apply the pattern.
    //
    // The cost is that a pattern rule sees only the first `SEARCH_FETCH` rows
    // of the whole catalogue rather than of its own subtree, so a narrow
    // pattern over a large registry can come back short. That is the same
    // truncation `tools_dropped` already reports for the cap, and the query
    // does the real selecting in practice.
    const rows = await searchRows(host, query, isGlob(rule.path) ? null : rule.path);

    for (const a of rows) {
      const ref = refOf(a.path, a.name);
      if (seen[ref]) continue;
      seen[ref] = true;
      if (known && known[ref]) continue;
      if (!permitted(a, allow)) continue;
      matched++;
      if (tools.length >= cap) continue;

      const toolName = encodeToolName(a.path, a.name);
      map[toolName] = ref;
      // The label is for the transcript, never the wire: `encodeToolName` keeps
      // its collision-free internal name for dispatch, and the caption (or the
      // plain action name) is what a person reads.
      labels[toolName] = (a.caption && a.caption.trim()) || a.name;
      tools.push({
        type: "function",
        function: {
          name: toolName,
          description: a.description || a.caption || "Execute " + ref,
          parameters: await schemaFor(host, a),
        },
      });
    }
  }

  return { tools, map, labels, dropped: Math.max(0, matched - tools.length) };
}

export function invertMap(map: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const key of Object.keys(map || {})) out[map[key]] = key;
  return out;
}

export interface PathSuggestion {
  path: string;
  count: number;
  sample?: string;
}

/**
 * Distinct action paths matching a partial query, for the setup panel's path
 * picker. `path` is itself indexed in `actions_fts`, so "firefox" already
 * matches "/packages/solx-firefox" without any special-casing here.
 *
 * Not run through the gate -- this only lists what a package *registered*.
 * That is what the setup panel needs: deciding what to grant requires seeing
 * what exists, before anything is granted.
 *
 * Two callers, and the second one matters. This was operator-only by design;
 * `sys__tool_search` now also uses it, on a failed search, to tell the model
 * which *paths* hold matches its grant does not cover. That is a deliberate
 * relaxation -- see `outOfGrantPaths`. It stays paths-only for the model:
 * this returns no tool names, and nothing here can be called.
 */
export async function searchActionPaths(
  host: Host,
  q: string,
  limit = 20,
): Promise<PathSuggestion[]> {
  const page = await host.try<{ items?: ActionRow[] }>(
    SEARCH_ACTIONS,
    compact({ q: q || null, limit: SEARCH_FETCH, excludeHidden: true }),
  );
  if (!page.ok || !page.value || !Array.isArray(page.value.items)) return [];

  const byPath = new Map<string, PathSuggestion>();
  for (const a of page.value.items) {
    const existing = byPath.get(a.path);
    if (existing) existing.count++;
    else byPath.set(a.path, { path: a.path, count: 1, sample: a.caption || a.description });
  }
  return Array.from(byPath.values())
    .sort((x, y) => x.path.localeCompare(y.path))
    .slice(0, limit);
}

/** Re-exported so skills can bind by glob without importing the gate directly. */
export { globMatches };

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

import { globMatches, permitted, refOf, splitRef } from "./gate";
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
  return "act__" + segments.join("__").replace(/[^A-Za-z0-9_]/g, "_");
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
  dropped: number;
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
  let matched = 0;

  for (const rule of allow) {
    const page = await host.try<{ items?: ActionRow[] }>(
      SEARCH_ACTIONS,
      // `q` is omitted rather than nulled when there is no query -- see
      // `compact`. A null there fails schema validation and the whole search
      // errors, which is how a no-query fallback silently resolves to nothing.
      compact({ q: query, pathPrefix: rule.path, limit: SEARCH_FETCH, excludeHidden: true }),
    );
    if (!page.ok || !page.value || !Array.isArray(page.value.items)) continue;

    for (const a of page.value.items) {
      const ref = refOf(a.path, a.name);
      if (seen[ref]) continue;
      seen[ref] = true;
      if (known && known[ref]) continue;
      if (!permitted(a, allow)) continue;
      matched++;
      if (tools.length >= cap) continue;

      const toolName = encodeToolName(a.path, a.name);
      map[toolName] = ref;
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

  return { tools, map, dropped: Math.max(0, matched - tools.length) };
}

export function invertMap(map: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const key of Object.keys(map || {})) out[map[key]] = key;
  return out;
}

/** Re-exported so skills can bind by glob without importing the gate directly. */
export { globMatches };

/**
 * The corpus index: what the planner is told exists, so it can copy an exact
 * path or type instead of inventing one.
 *
 * This is inquiry's replacement for the agent's tool catalogue, and it is the
 * single most important difference between the two. The agent's catalogue is a
 * *dispatch table* -- names the model may call. This is *vocabulary* -- words
 * the model may reuse. Nothing in it is callable, which is why there is no
 * gate: the worst a model can do with a path it read here is search under it.
 *
 * Two capped list calls, no full-text query. `entity-list-document-paths`
 * returns distinct paths with counts, which is exactly the shape wanted and
 * avoids enumerating every document; `entity-list-types` gives the types. The
 * whole thing is a few hundred characters injected into the planner prompt,
 * so it is capped by prompt budget rather than by store size.
 */

import { compact } from "./host";
import { truncate } from "./text";
import {
  CORPUS_INDEX_CAP,
  CORPUS_PATH_LIMIT,
  CORPUS_TYPE_LIMIT,
  LIST_DOC_PATHS,
  LIST_TYPES,
  SEARCH_ACTIONS,
} from "./refs";
import type { Host } from "./host";

interface PathRow {
  path?: string;
  count?: number;
}

/**
 * Fetch distinct document paths, most-populated first.
 *
 * `entity-list-document-paths` is the right call rather than a document list:
 * it is one row per path, so a store with ten thousand documents returns the
 * same handful of paths a store with ten does, and the planner needs the
 * handful. It fails soft -- a host without the action simply yields no paths,
 * and the planner searches unscoped.
 *
 * Takes a plain `ListOptions`, so there is deliberately no `excludeHidden`
 * here: that field exists only on `ActionSearchQuery`, and on a `ListOptions`
 * schema it would be *silently dropped* -- the exact failure mode this
 * package's refs.ts warns about. Nothing needs filtering anyway; documents
 * have no hidden flag.
 */
export async function resolveCorpusPaths(host: Host, limit = CORPUS_PATH_LIMIT): Promise<PathRow[]> {
  const r = await host.try<{ items?: PathRow[] }>(LIST_DOC_PATHS, compact({ limit }));
  if (!r.ok || !r.value || !Array.isArray(r.value.items)) return [];
  return r.value.items
    .filter((p) => typeof p.path === "string" && p.path !== "")
    .sort((a, b) => (b.count || 0) - (a.count || 0))
    .slice(0, limit);
}

/** Fetch type names, so the planner can name a kind of document exactly. Also a plain `ListOptions`. */
export async function resolveCorpusTypes(host: Host, limit = CORPUS_TYPE_LIMIT): Promise<string[]> {
  const r = await host.try<{ items?: { path?: string; name?: string; title?: string }[] }>(
    LIST_TYPES,
    compact({ limit }),
  );
  if (!r.ok || !r.value || !Array.isArray(r.value.items)) return [];
  return r.value.items
    .map((t) => t.name || t.title || "")
    .filter((n) => n !== "")
    .slice(0, limit);
}

/**
 * Fetch action paths, only when the operator asked for them.
 *
 * This one *does* carry `excludeHidden`, because `search-actions` deserializes
 * an `ActionSearchQuery`, whose `exclude_hidden` field is real -- hidden
 * actions are dropped before the planner could reuse a path from one.
 */
export async function resolveCorpusActions(host: Host, limit = CORPUS_PATH_LIMIT): Promise<string[]> {
  const r = await host.try<{ items?: { path?: string }[] }>(
    SEARCH_ACTIONS,
    compact({ limit, excludeHidden: true }),
  );
  if (!r.ok || !r.value || !Array.isArray(r.value.items)) return [];
  const seen: Record<string, boolean> = {};
  const out: string[] = [];
  for (const a of r.value.items) {
    const p = a.path || "";
    if (p === "" || seen[p]) continue;
    seen[p] = true;
    out.push(p);
    if (out.length >= limit) break;
  }
  return out;
}

export interface Corpus {
  paths: PathRow[];
  types: string[];
  actions: string[];
  /** The rendered block injected into the planner prompt. */
  text: string;
}

/**
 * Build and render the index. `includeActions` is off by default: an action
 * path is only useful when the question is "what can this system do", which
 * is rare, and the extra lines are prompt tokens on every plan.
 */
export async function buildCorpus(host: Host, includeActions: boolean): Promise<Corpus> {
  const paths = await resolveCorpusPaths(host);
  const types = await resolveCorpusTypes(host);
  const actions = includeActions ? await resolveCorpusActions(host) : [];
  return { paths, types, actions, text: renderCorpus({ paths, types, actions }) };
}

/** Render the index as the compact block the planner reads. Capped, then truncated. */
export function renderCorpus(corpus: { paths: PathRow[]; types: string[]; actions: string[] }): string {
  const lines: string[] = [];

  if (corpus.paths.length > 0) {
    lines.push("Document paths in this solx instance (path: how many documents):");
    for (const p of corpus.paths) lines.push("  " + p.path + ": " + (p.count ?? 0));
  } else {
    lines.push("Document paths in this solx instance: (none listed)");
  }

  if (corpus.types.length > 0) {
    lines.push("");
    lines.push("Document types: " + corpus.types.join(", "));
  }

  if (corpus.actions.length > 0) {
    lines.push("");
    lines.push("Action paths: " + corpus.actions.join(", "));
  }

  return truncate(lines.join("\n"), CORPUS_INDEX_CAP);
}

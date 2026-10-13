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
 * A capped path list and a small per-path type sample, no full-text query.
 * `entity-list-document-paths` returns distinct paths with counts, which is
 * exactly the shape wanted and avoids enumerating every document; one row from
 * each of the top few paths names the types documents actually carry. The
 * whole thing is a few hundred characters injected into the planner prompt,
 * so it is capped by prompt budget rather than by store size.
 */

import { compact } from "./host";
import { truncate } from "./text";
import {
  CORPUS_INDEX_CAP,
  CORPUS_PATH_LIMIT,
  CORPUS_TYPE_LIMIT,
  CORPUS_TYPE_SAMPLE_PATHS,
  INQUIRY_ROOT,
  LIST_DOC_PATHS,
  SEARCH_ACTIONS,
  SEARCH_DOCS,
  TOOLING,
} from "./refs";
import type { Host } from "./host";
import type { Vocabulary } from "./execute";

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
 * Asks for the tooling scope like every other read here, so a path the
 * operator excluded from tooling never reaches the planner. (`excludeMode`
 * is a real `ListOptions` field; the old `excludeHidden` was not, and would
 * have been silently dropped.)
 */
export async function resolveCorpusPaths(host: Host, limit = CORPUS_PATH_LIMIT): Promise<PathRow[]> {
  const r = await host.try<{ items?: PathRow[] }>(
    LIST_DOC_PATHS,
    compact({ limit, excludeMode: TOOLING }),
  );
  if (!r.ok || !r.value || !Array.isArray(r.value.items)) return [];
  return r.value.items
    .filter((p) => typeof p.path === "string" && p.path !== "")
    .sort((a, b) => (b.count || 0) - (a.count || 0))
    .slice(0, limit);
}

/**
 * The types documents actually carry, as full refs, so the planner can copy one
 * into `type_ref` and have it match.
 *
 * Not `entity-list-types`, which is what this used to read and what running the
 * package showed to be actively harmful. The type registry is dominated by
 * builtin action-*parameter* types (`SearchDocumentsParams`, ...) that no
 * document ever has, and it came back as bare names -- so a model told to copy
 * a type "exactly as written" sent `typeRef: "SearchDocumentsParams"` and the
 * filter matched nothing. Vocabulary that cannot be used is worse than none.
 *
 * There is no type facet on documents, and a document row carries its whole
 * body, so this samples one row at each of the most populated paths rather than
 * listing documents broadly. The package's own `/inquiry` tree is skipped: its
 * rows are the heaviest in the store and its type is known.
 */
export async function resolveCorpusTypes(
  host: Host,
  paths: PathRow[],
  limit = CORPUS_TYPE_LIMIT,
): Promise<string[]> {
  const seen: Record<string, boolean> = {};
  const out: string[] = [];
  const sample = paths
    .filter((p) => p.path && p.path !== INQUIRY_ROOT && !p.path!.startsWith(INQUIRY_ROOT + "/"))
    .slice(0, CORPUS_TYPE_SAMPLE_PATHS);
  for (const p of sample) {
    const r = await host.try<{ items?: { typeRef?: string; type_ref?: string }[] }>(
      SEARCH_DOCS,
      compact({ pathPrefix: p.path, limit: 1, excludeMode: TOOLING }),
    );
    if (!r.ok || !r.value || !Array.isArray(r.value.items)) continue;
    for (const row of r.value.items) {
      const ref = row.typeRef || row.type_ref || "";
      if (ref === "" || seen[ref]) continue;
      seen[ref] = true;
      out.push(ref);
    }
    if (out.length >= limit) break;
  }
  return out.slice(0, limit);
}

/**
 * Fetch action paths, only when the operator asked for them.
 *
 * In the tooling scope, so hidden actions (config rules and the row's own
 * `solx:hidden` tag) are dropped before the planner could reuse a path from
 * one.
 */
export async function resolveCorpusActions(host: Host, limit = CORPUS_PATH_LIMIT): Promise<string[]> {
  const r = await host.try<{ items?: { path?: string }[] }>(
    SEARCH_ACTIONS,
    compact({ limit, excludeMode: TOOLING }),
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
  const types = await resolveCorpusTypes(host, paths);
  const actions = includeActions ? await resolveCorpusActions(host) : [];
  return { paths, types, actions, text: renderCorpus({ paths, types, actions }) };
}

/** The scopes the index showed, for `executePlan`'s repair and widening. */
export function vocabularyOf(corpus: Corpus): Vocabulary {
  return { paths: corpus.paths.map((p) => p.path || "").filter((p) => p !== ""), types: corpus.types };
}

/** Render the index as the compact block the planner reads. Capped, then truncated. */
export function renderCorpus(corpus: { paths: PathRow[]; types: string[]; actions: string[] }): string {
  const lines: string[] = [];

  // The package's own transcripts are not offered as a place to look. Listed,
  // they read as one more scope, and a planner with nothing better reaches for
  // them -- observed live on a question the store could not answer. The brief
  // says where they are, for the question that is genuinely about them.
  const paths = corpus.paths.filter(
    (p) => p.path !== INQUIRY_ROOT && !(p.path || "").startsWith(INQUIRY_ROOT + "/"),
  );
  if (paths.length > 0) {
    lines.push("Document paths in this solx instance (path: how many documents):");
    for (const p of paths) lines.push("  " + p.path + ": " + (p.count ?? 0));
  } else {
    lines.push("Document paths in this solx instance: (none listed)");
  }

  if (corpus.types.length > 0) {
    lines.push("");
    lines.push("Document types in use (for type_ref, not paths): " + corpus.types.join(", "));
  }

  if (corpus.actions.length > 0) {
    lines.push("");
    lines.push("Action paths: " + corpus.actions.join(", "));
  }

  return truncate(lines.join("\n"), CORPUS_INDEX_CAP);
}

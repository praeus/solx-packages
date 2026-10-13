/**
 * Running a validated plan. No model, no gate, no approval.
 *
 * This is the second half of the entity optimization: once the planner has
 * named a verb and some words, everything else is deterministic. Each step
 * maps to one read-only builtin through `VERB_ACTION`, the params are coerced
 * to the wire's camelCase and `compact()`ed (an explicit null fails schema
 * validation -- the `q: null` trap `solx-agent`'s DESIGN.md records), and the
 * results are gathered into a grounding set.
 *
 * Three budgets bound the output: a per-step `limit`, a total document count,
 * and -- in `answer.ts` -- a total character budget. They are separate because
 * they fail differently: too many documents crowd the answer prompt, too few
 * characters starve it, and a single huge document should not be able to spend
 * the whole budget on its own.
 */

import { compact } from "./host";
import { clamp } from "./text";
import {
  DEFAULT_MAX_DOCS,
  DEFAULT_STEP_LIMIT,
  INQUIRY_ROOT,
  MAX_DOCS_HARD,
  MAX_STEP_LIMIT,
  refOf,
  TYPE_FETCH,
  VERB_ACTION,
} from "./refs";
import type { ExecutedStep, GatheredDoc, PlanStep } from "./types";
import type { Host } from "./host";

/**
 * A row from any of the three registries the verbs read. Documents, types and
 * actions all share the same `(path, name)` identity, so one shape absorbs
 * them; the difference is only which descriptive fields are present.
 */
interface RowHit {
  path?: string;
  name?: string;
  title?: string;
  caption?: string;
  description?: string;
  summary?: string;
  typeRef?: string;
  type_ref?: string;
  contents?: unknown;
  schema?: unknown;
}

/**
 * The wire params for one step, in the spelling the builtins expect.
 *
 * The one real trap: solx's `entity-get-document` takes the two halves as
 * `path` and `name` (an `EntityRefParams`), while the verbs carry them as
 * `path`/`name` too -- so `get` passes through -- but search/list take
 * camelCase `pathPrefix`/`typeRef`. Every key here was checked against the
 * struct that reads it (`SearchQuery`, `ListOptions`), because a misspelled
 * key is dropped in silence.
 */
function paramsFor(step: PlanStep): Record<string, unknown> {
  const limit = clamp(step.limit ?? DEFAULT_STEP_LIMIT, 1, MAX_STEP_LIMIT);

  if (step.verb === "get") {
    return compact({ path: step.path || "/", name: step.name || null });
  }

  if (step.verb === "types") {
    // There is no `search-types` builtin, so a keyword search over types is a
    // capped list filtered here. The list is asked for generously because the
    // filtering happens after the fetch.
    return compact({ pathPrefix: "/", limit: TYPE_FETCH });
  }

  if (step.verb === "actions") {
    return compact({ q: step.q || null, limit });
  }

  // search and list are the same builtin; list simply passes no `q`.
  return compact({
    q: step.verb === "list" ? null : step.q || null,
    pathPrefix: step.path_prefix || null,
    typeRef: step.type_ref || null,
    limit,
  });
}

/**
 * A row into a `GatheredDoc`.
 *
 * Types and actions are not documents, but they ground an answer the same way
 * -- "this is what the system holds / can do" -- so they travel in the same
 * shape. A type's schema and an action's description become the body, which is
 * what makes `types`/`actions` answers possible without a special case in the
 * answer phase.
 */
function toDoc(hit: RowHit, kind: "document" | "type" | "action"): GatheredDoc | null {
  const name = hit.name || "";
  if (!name) return null;
  const path = hit.path || "/";

  let body: unknown = hit.contents ?? null;
  if (kind === "type" && hit.schema !== undefined) body = hit.schema;
  if (kind === "action") body = hit.description || hit.caption || null;

  return {
    ref: refOf(path, name),
    path,
    name,
    title: hit.title || hit.caption || name,
    summary: hit.summary || hit.description || "",
    type_ref: hit.typeRef || hit.type_ref || null,
    contents: body,
  };
}

/** Pull `items` (search/list) or a single object (get) into GatheredDocs. */
function docsFrom(value: unknown, kind: "document" | "type" | "action"): GatheredDoc[] {
  if (!value || typeof value !== "object") return [];
  const v = value as Record<string, unknown>;

  if (Array.isArray(v.items)) {
    const docs: GatheredDoc[] = [];
    for (const item of v.items) {
      const doc = toDoc(item as RowHit, kind);
      if (doc) docs.push(doc);
    }
    return docs;
  }

  // A get returns one document, either bare or wrapped.
  const bare = toDoc(v as RowHit, kind);
  if (bare) return [bare];

  const inner = (v.result ?? v.document ?? v.contents) as RowHit | undefined;
  if (inner && typeof inner === "object") {
    const doc = toDoc(inner, kind);
    if (doc) return [doc];
  }
  return [];
}

/** Which registry a step reads, so a row is interpreted the right way. */
function kindOf(verb: string): "document" | "type" | "action" {
  if (verb === "types") return "type";
  if (verb === "actions") return "action";
  return "document";
}

/**
 * Filter a fetched type list down to those whose name or path contains any of
 * the step's terms. Case-insensitive substring, not FTS: the type registry has
 * no full-text index, and a small model's terms are approximate anyway.
 */
function filterTypes(docs: GatheredDoc[], q: string | null | undefined, limit: number): GatheredDoc[] {
  if (!q) return docs.slice(0, limit);
  const terms = q.toLowerCase().split(/\s+/).filter((t) => t.length > 1);
  if (terms.length === 0) return docs.slice(0, limit);
  const hits = docs.filter((d) => {
    const hay = (d.name + " " + d.path).toLowerCase();
    return terms.some((t) => hay.includes(t));
  });
  // Fall back to the unfiltered list when nothing matched, so a bad term
  // yields an overview rather than a dead end.
  return (hits.length > 0 ? hits : docs).slice(0, limit);
}

export interface ExecuteResult {
  steps: ExecutedStep[];
  documents: GatheredDoc[];
}

/**
 * Is this row the package's own transcript rather than content to answer from?
 *
 * A genuine issue, not a precaution: an inquiry's session document stores its
 * plan, and a plan contains the search terms. So a search for "zzznomatch"
 * *finds the inquiry document that is asking for it*, because the term is
 * sitting in that document's stored contents. Left unfiltered, every search
 * returns the thread itself alongside the real hits, crowding the answer's
 * budget with the model's own words and, worse, letting the writer treat its
 * own previous plan as source material.
 *
 * The filter is bypassed when the step deliberately scopes to the inquiry
 * root -- asking "what have I asked before?" is a legitimate question -- so
 * this is a default rather than a wall. Same spirit as solx-agent reserving
 * `/agent`, but implemented here rather than as a gate, because there is no
 * gate to implement it in.
 */
function isOwnTranscript(doc: GatheredDoc, pathPrefix: string | null | undefined): boolean {
  if (pathPrefix && (pathPrefix === INQUIRY_ROOT || pathPrefix.startsWith(INQUIRY_ROOT + "/"))) {
    return false;
  }
  return doc.path === INQUIRY_ROOT || doc.path.startsWith(INQUIRY_ROOT + "/");
}

/**
 * Run every step in order, de-duplicating by ref and stopping once the
 * document budget is reached. A step that errors is recorded and skipped; the
 * turn continues, because one failed lookup should not discard the ones that
 * worked -- and a partial grounding set is still an answer's worth.
 */
export async function executePlan(
  host: Host,
  steps: PlanStep[],
  maxDocs = DEFAULT_MAX_DOCS,
): Promise<ExecuteResult> {
  const cap = clamp(maxDocs, 1, MAX_DOCS_HARD);
  const out: ExecutedStep[] = [];
  const documents: GatheredDoc[] = [];
  const seen: Record<string, boolean> = {};

  for (const step of steps) {
    const action = VERB_ACTION[step.verb];
    const params = paramsFor(step);

    if (!action) {
      out.push({
        verb: step.verb,
        action: "",
        params,
        outcome: "skipped",
        count: 0,
        error: "no action mapped for verb " + step.verb,
        note: step.note ?? null,
      });
      continue;
    }

    const r = await host.try(action, params);
    if (!r.ok) {
      out.push({
        verb: step.verb,
        action,
        params,
        outcome: "error",
        count: 0,
        error: r.error,
        note: step.note ?? null,
      });
      continue;
    }

    const kind = kindOf(step.verb);
    let candidates = docsFrom(r.value, kind);
    if (kind === "type") {
      candidates = filterTypes(candidates, step.q, clamp(step.limit ?? DEFAULT_STEP_LIMIT, 1, MAX_STEP_LIMIT));
    }
    candidates = candidates.filter((d) => !isOwnTranscript(d, step.path_prefix));

    let contributed = 0;
    for (const doc of candidates) {
      if (seen[doc.ref]) continue;
      if (documents.length >= cap) break;
      seen[doc.ref] = true;
      documents.push(doc);
      contributed++;
    }

    out.push({
      verb: step.verb,
      action,
      params,
      outcome: contributed === 0 ? "empty" : "ok",
      count: contributed,
      error: null,
      note: step.note ?? null,
    });
  }

  return { steps: out, documents };
}

/** True when every step ran but nothing at all came back -- the replan trigger. */
export function foundNothing(result: ExecuteResult): boolean {
  return result.documents.length === 0;
}

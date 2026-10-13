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
 * Is this a row the package's own transcript rather than content to answer
 * from?
 *
 * A genuine issue, not a precaution: an inquiry's session document stores its
 * plan and its question, and both contain the search terms. So a search for
 * "what does documents say about save" *finds the inquiry document that is
 * asking for it*, because those words are sitting in that document's stored
 * contents. Left unfiltered, every search returns the thread itself alongside
 * the real hits, crowding the answer's budget with the model's own words and,
 * worse, letting the writer treat its own previous question as source
 * material.
 *
 * The filter is bypassed when the step deliberately scopes to the inquiry
 * root -- asking "what have I asked before?" is a legitimate question -- so
 * this is a default rather than a wall.
 */
function isOwnPath(path: string, pathPrefix: string | null | undefined): boolean {
  if (pathPrefix && (pathPrefix === INQUIRY_ROOT || pathPrefix.startsWith(INQUIRY_ROOT + "/"))) {
    return false;
  }
  return path === INQUIRY_ROOT || path.startsWith(INQUIRY_ROOT + "/");
}

/** How many of a page's rows are usable content rather than own transcript. */
function usableCount(items: unknown[], pathPrefix: string | null | undefined): number {
  let n = 0;
  for (const item of items) {
    const path = (item as { path?: string } | null)?.path || "/";
    if (!isOwnPath(path, pathPrefix)) n++;
  }
  return n;
}

function itemsOf(value: unknown): unknown[] {
  const items = (value as { items?: unknown[] } | null)?.items;
  return Array.isArray(items) ? items : [];
}

/**
 * Split a query into the terms a retry would try individually.
 *
 * Mirrors the term extraction `solx-agent`'s catalogue search uses for the
 * same reason: it strips punctuation and drops single characters, because a
 * one-letter term matches almost everything and is worse than useless as a
 * widening step.
 */
function searchTerms(q: string): string[] {
  return q
    .split(/\s+/)
    .map((t) => t.replace(/[^\p{L}\p{N}_-]/gu, ""))
    .filter((t) => t.length > 1);
}

/**
 * Fetch one step's rows, retrying term-by-term when the phrase matched nothing.
 *
 * This is the fix for the failure that running the package exposed. `q` goes
 * to solx-docs' `fts_match_query`, which turns each whitespace-separated term
 * into `"term"*` and **ANDs** them — so a phrase only matches a document whose
 * text contains *every* word. A model asked for "two or three distinctive
 * words" writes a sentence anyway: the observed query was "documents skill how
 * a document is identified identifier", which ANDs to nothing and returns an
 * empty page that looks exactly like "no such document exists".
 *
 * The widening lives here rather than in the engine because AND is the right
 * default for document search, where precision matters. `solx-agent` reached
 * the same conclusion for its tool catalogue and did the same thing; a
 * sentence-shaped `q` is the shape of input that breaks both, so both need it.
 *
 * Only a *zero* result triggers the retry: a phrase that matched something
 * matched it precisely, and that ranking beats anything a merge reconstructs.
 * The merge is round-robin by rank, so each term contributes its best matches
 * before any term contributes its worst. A retry that also finds nothing
 * returns the empty page, leaving the honest "no such document" outcome.
 */
async function searchWithFallback(
  host: Host,
  action: string,
  params: Record<string, unknown>,
  q: string | null | undefined,
  pathPrefix: string | null | undefined,
): Promise<{ ok: true; value: unknown } | { ok: false; error: string }> {
  const first = await host.try(action, params);
  if (!first.ok || !q) return first;

  // Judge on *usable* rows, not the raw page. This is the second half of the
  // own-transcript bug, and the subtle one: the phrase "overwrite existing
  // name save document" genuinely matches -- but only the inquiry document
  // whose stored question contains exactly those words. The raw count is
  // therefore non-zero, the widening never fires, and then the filter below
  // removes every match and the search is reported empty. Counting what can
  // actually ground an answer is what makes the retry reach for the real
  // document underneath.
  if (usableCount(itemsOf(first.value), pathPrefix) > 0) return first;

  const terms = searchTerms(q);
  if (terms.length < 2) return first;

  const pages: { items?: unknown[] }[] = [];
  for (const term of terms) {
    const page = await host.try<{ items?: unknown[] }>(action, { ...params, q: term });
    if (page.ok && page.value) pages.push(page.value);
  }
  if (pages.length === 0) return first;

  const merged: unknown[] = [];
  const limit = typeof params.limit === "number" ? params.limit : 10;
  const seenRef = new Set<string>();
  for (let rank = 0; merged.length < limit; rank++) {
    let anyLeft = false;
    for (const page of pages) {
      const items = page.items || [];
      if (rank >= items.length) continue;
      anyLeft = true;
      const row = items[rank] as { path?: string; name?: string };
      // Dedup on the same (path, name) identity the document store uses, so a
      // row matched by two terms is not gathered twice.
      const key = (row?.path || "/") + "/" + (row?.name || "");
      if (seenRef.has(key)) continue;
      seenRef.add(key);
      merged.push(row);
      if (merged.length >= limit) break;
    }
    if (!anyLeft) break;
  }

  return { ok: true, value: { ...(first.value as object), items: merged } };
}

/**
 * What the corpus index showed the planner: the scopes that are known to exist.
 * Optional -- without it no repair is attempted, only the empty-result widening
 * of a type filter.
 */
export interface Vocabulary {
  paths: string[];
  types: string[];
}

function isUnder(path: string, prefix: string): boolean {
  return prefix === "/" || path === prefix || path.startsWith(prefix.replace(/\/$/, "") + "/");
}

/**
 * Fix a step's scope before it runs, using what the corpus index said exists.
 *
 * Both repairs are for mistakes running the package showed a real model makes
 * when told to "copy a path or type from the list":
 *
 * - it puts a *document path* in `type_ref` (`type_ref: "/agent/skills"`). A
 *   type filter that is really a path matches nothing; as a `path_prefix` it
 *   is exactly what was meant.
 * - it copies a type's bare name (`AgentSkill`) rather than its ref. Resolved
 *   when exactly one known type has that name.
 *
 * Returns a copy and a note; never throws, in keeping with `plan.ts`.
 */
function repairScope(step: PlanStep, vocab: Vocabulary | undefined): { step: PlanStep; note: string | null } {
  if (!vocab || !step.type_ref || vocab.types.includes(step.type_ref)) return { step, note: null };
  const t = step.type_ref;

  const asPath = t.startsWith("/") && vocab.paths.some((p) => isUnder(p, t));
  if (asPath) {
    const fixed = { ...step, type_ref: null };
    if (!fixed.path_prefix) fixed.path_prefix = t;
    return { step: fixed, note: "type_ref " + t + " is a document path; used it as path_prefix" };
  }

  const bare = t.slice(t.lastIndexOf("/") + 1).toLowerCase();
  const byName = vocab.types.filter((ref) => ref.slice(ref.lastIndexOf("/") + 1).toLowerCase() === bare);
  if (byName.length === 1) {
    return { step: { ...step, type_ref: byName[0] }, note: "type_ref " + t + " resolved to " + byName[0] };
  }
  return { step, note: null };
}

/**
 * The filters a scoped search may shed when it found nothing usable, in the
 * order they are shed.
 *
 * A filter is only shed when it is suspect: a type the corpus did not list
 * (or any type, when there is no vocabulary to check against), or a path
 * prefix that covers no known path. A *valid* scope that found nothing is a
 * real answer -- "nothing about that under /blog" -- and widening it would
 * swap that answer for hits from elsewhere. Shedding is a cheap deterministic
 * lookup, where the alternative is the pipeline's replan: a whole model call.
 */
function widenings(step: PlanStep, vocab: Vocabulary | undefined): ("typeRef" | "pathPrefix")[] {
  const out: ("typeRef" | "pathPrefix")[] = [];
  if (step.type_ref && (!vocab || !vocab.types.includes(step.type_ref))) out.push("typeRef");
  if (
    step.path_prefix &&
    vocab &&
    vocab.paths.length > 0 &&
    !vocab.paths.some((p) => isUnder(p, step.path_prefix!) || isUnder(step.path_prefix!, p))
  ) {
    out.push("pathPrefix");
  }
  return out;
}

/**
 * Run every step in order, de-duplicating by ref and stopping once the
 * document budget is reached. A step that errors is recorded and skipped; the
 * turn continues, because one failed lookup should not discard the ones that
 * worked -- and a partial grounding set is still an answer's worth.
 *
 * `exclude` names refs that are never gathered, whatever the scope: the
 * pipeline passes the inquiry's own document. The `/inquiry` bypass exists for
 * "what have I asked before?", which is about *other* inquiries -- the one
 * being written holds this question verbatim, so it always matches, and
 * handing it to the writer is the own-transcript bug through a side door.
 * Found live: a replan scoped to `/inquiry/sessions` gathered itself.
 */
export async function executePlan(
  host: Host,
  steps: PlanStep[],
  maxDocs = DEFAULT_MAX_DOCS,
  vocab?: Vocabulary,
  exclude: string[] = [],
): Promise<ExecuteResult> {
  const cap = clamp(maxDocs, 1, MAX_DOCS_HARD);
  const out: ExecutedStep[] = [];
  const documents: GatheredDoc[] = [];
  const seen: Record<string, boolean> = {};

  for (const planned of steps) {
    const repaired = repairScope(planned, vocab);
    const step = repaired.step;
    const notes: string[] = [];
    if (planned.note) notes.push(planned.note);
    if (repaired.note) notes.push(repaired.note);
    const noteText = () => (notes.length > 0 ? notes.join("; ") : null);

    const action = VERB_ACTION[step.verb];
    let params = paramsFor(step);

    if (!action) {
      out.push({
        verb: step.verb,
        action: "",
        params,
        outcome: "skipped",
        count: 0,
        error: "no action mapped for verb " + step.verb,
        note: noteText(),
      });
      continue;
    }

    // Only the document search takes terms; `list` sends no `q` at all, and
    // the type list is filtered client-side, so neither gets the retry.
    const usesTerms = step.verb === "search" && !!step.q;
    let r = usesTerms
      ? await searchWithFallback(host, action, params, step.q, step.path_prefix)
      : await host.try(action, params);

    // A scoped document lookup that found nothing usable sheds a suspect
    // filter and tries again -- see `widenings`.
    // The scope that actually ran, which the own-transcript filter below must
    // judge rows by -- it changes if `pathPrefix` is shed.
    let scope = step.path_prefix;
    if ((step.verb === "search" || step.verb === "list") && r.ok) {
      for (const drop of widenings(step, vocab)) {
        if (usableCount(itemsOf(r.value), scope) > 0) break;
        // Without terms, shedding the path would just enumerate the store.
        if (drop === "pathPrefix" && !usesTerms) break;
        const wider: Record<string, unknown> = { ...params };
        delete wider[drop];
        const widerScope = drop === "pathPrefix" ? null : scope;
        const retried = usesTerms
          ? await searchWithFallback(host, action, wider, step.q, widerScope)
          : await host.try(action, wider);
        if (!retried.ok) break;
        r = retried;
        params = wider;
        scope = widerScope;
        notes.push(
          "found nothing with " + (drop === "typeRef" ? "type_ref " + step.type_ref : "path_prefix " + step.path_prefix) +
            "; searched without it",
        );
      }
    }

    if (!r.ok) {
      out.push({
        verb: step.verb,
        action,
        params,
        outcome: "error",
        count: 0,
        error: r.error,
        note: noteText(),
      });
      continue;
    }

    const kind = kindOf(step.verb);
    let candidates = docsFrom(r.value, kind);
    if (kind === "type") {
      candidates = filterTypes(candidates, step.q, clamp(step.limit ?? DEFAULT_STEP_LIMIT, 1, MAX_STEP_LIMIT));
    }
    candidates = candidates.filter((d) => !isOwnPath(d.path, scope) && !exclude.includes(d.ref));

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
      note: noteText(),
    });
  }

  return { steps: out, documents };
}

/** True when every step ran but nothing at all came back -- the replan trigger. */
export function foundNothing(result: ExecuteResult): boolean {
  return result.documents.length === 0;
}

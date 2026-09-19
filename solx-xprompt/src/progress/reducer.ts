/**
 * Folding a run's progress events into the state a UI renders.
 *
 * A pure function of the console entries seen so far, deliberately: the tail
 * loop re-reads a run from its own `consoleSeqStart` after a reload, so a fold
 * over the whole retained buffer reconstructs exactly the state a live session
 * had, with nothing to resume or persist. Incremental folding would be faster
 * and would not have that property.
 *
 * Everything here is written for a feed that can lose, duplicate, reorder and
 * truncate its events — see `events.ts` for why each of those is a normal
 * occurrence rather than a fault:
 *
 * * an inquiry **never moves backwards** through its phases, which absorbs
 *   duplicate delivery, out-of-order arrival, and the deliberate
 *   `finished`/`failed` pair for one failure, in any order;
 * * an unseen inquiry index is **created on sight**, so a lost
 *   `inquiry.started` costs the question text rather than the whole row;
 * * `expected` only ever **grows**, so a lost count does not shrink the strip;
 * * a run that is first seen mid-flight still produces sane state, because
 *   nothing is inferred from an event's absence.
 */

import type { MergedEntry } from "../console";
import { parseProgressEvent, type IntentMode, type ProgressEvent, type Scope } from "./events";

/** Where one inquiry has got to. Ordered — see [`RANK`]. */
export type InquiryPhase = "planned" | "searched" | "running" | "done" | "failed";

/**
 * The one-way ladder. A phase is applied only when it ranks *above* the phase
 * already recorded, which is what makes every invariant above hold without
 * any per-event special-casing.
 */
const RANK: Record<InquiryPhase, number> = {
  planned: 0,
  searched: 1,
  running: 2,
  done: 3,
  failed: 4,
};

export interface InquiryProgress {
  index: number;
  kind?: Scope;
  question?: string;
  hits?: number;
  phase: InquiryPhase;
  /** The raw terminal status word, when one was reported (`ok`, `timeout`, …). */
  status?: string;
  reason?: string;
}

/** Which part of the pipeline the run is in. */
export type Stage =
  | "idle"
  | "recall"
  | "context"
  | "intent"
  | "fanout"
  | "assembling"
  | "done"
  | "failed"
  | "cancelled";

export interface ProgressState {
  /** The invocation these events belong to; `null` before anything was seen. */
  invocationId: string | null;
  stage: Stage;
  mode?: IntentMode;
  /** How many inquiries this run should end up with, as best known. */
  expected?: number;
  /** Set when the fan-out is running one inquiry at a time; the reason why. */
  degraded?: string;
  /** Dense and ordered by index — one entry per inquiry seen. */
  inquiries: InquiryProgress[];
  /** Why the run ended, when it ended badly. */
  reason?: string;
}

export function emptyProgress(): ProgressState {
  return { invocationId: null, stage: "idle", inquiries: [] };
}

/** True once the run has stopped, however it stopped. */
export function isSettled(state: ProgressState): boolean {
  return state.stage === "done" || state.stage === "failed" || state.stage === "cancelled";
}

/**
 * Fold every entry belonging to `activeInvocationId` into one state.
 *
 * Entries from other invocations — earlier turns in the same session, which
 * share this call log — are skipped rather than reset over, so switching the
 * active turn does not depend on the order the merge happened to interleave
 * them in.
 */
export function foldProgress(
  entries: MergedEntry[],
  activeInvocationId: string | null,
): ProgressState {
  let state = emptyProgress();
  if (!activeInvocationId) return state;
  for (const entry of entries) {
    if (entry.invocationId !== activeInvocationId) continue;
    const event = parseProgressEvent(entry);
    if (event) state = reduceProgress(state, event, entry.invocationId);
  }
  return state;
}

export function reduceProgress(
  state: ProgressState,
  event: ProgressEvent,
  invocationId: string,
): ProgressState {
  // A different run entirely: start over rather than blending two turns'
  // progress. This is the only reset there is — no caller has to clear.
  const base =
    state.invocationId && state.invocationId !== invocationId ? emptyProgress() : state;
  const next: ProgressState = { ...base, invocationId, inquiries: base.inquiries };

  switch (event.t) {
    case "run.started":
      // Deliberately does not seed `expected`: this event's `n` is the run's
      // *cap* (`max_inquiries`), not a count of anything proposed, and sizing
      // the strip from it would show three slots before the intent phase has
      // decided there are any.
      return { ...next, stage: "recall" };
    case "recall.done":
      return { ...next, stage: advance(next.stage, "recall") };
    case "context.done":
      return { ...next, stage: advance(next.stage, "context") };
    case "intent.started":
      return { ...next, stage: advance(next.stage, "intent") };
    case "intent.done":
      return {
        ...next,
        stage: advance(next.stage, event.mode === "direct" ? "assembling" : "fanout"),
        // A direct answer opens no fan-out row at all, and a consumer reads
        // that off the mode rather than inferring it from the absence of
        // inquiry events. With `n: 0` this also settles `expected` at zero.
        mode: event.mode,
        expected: grow(next.expected, event.n),
      };
    case "intent.failed":
      return { ...next, stage: "failed", reason: event.reason };
    case "fanout.started":
      return { ...next, stage: advance(next.stage, "fanout"), expected: grow(next.expected, event.n) };
    case "fanout.degraded":
      return {
        ...next,
        stage: advance(next.stage, "fanout"),
        expected: grow(next.expected, event.n),
        degraded: event.reason ?? "sequential",
      };
    case "inquiry.planned":
      return withInquiry(next, event.i, (one) => ({
        ...one,
        phase: raise(one.phase, "planned"),
        kind: event.kind ?? one.kind,
        question: event.q ?? one.question,
      })).withExpected(event.n);
    case "inquiry.searched":
      return withInquiry(next, event.i, (one) => ({
        ...one,
        phase: raise(one.phase, "searched"),
        hits: event.hits ?? one.hits,
      })).state;
    case "inquiry.started":
      return withInquiry(next, event.i, (one) => ({
        ...one,
        phase: raise(one.phase, "running"),
        kind: event.kind ?? one.kind,
        question: event.q ?? one.question,
      })).withExpected(event.n);
    case "inquiry.finished":
      return withInquiry(next, event.i, (one) => ({
        ...one,
        // A failure reported as `finished { ok: false }` and again as
        // `inquiry.failed` lands on the same phase whichever arrives first.
        phase: raise(one.phase, event.ok ? "done" : "failed"),
        status: event.status ?? one.status,
      })).state;
    case "inquiry.failed":
      return withInquiry(next, event.i, (one) => ({
        ...one,
        phase: raise(one.phase, "failed"),
        reason: event.reason ?? one.reason,
      })).state;
    // Terminal stages are assigned directly rather than through `advance()`,
    // since none of "done"/"failed"/"cancelled" outranks another in
    // STAGE_RANK. Guard explicitly instead: once settled, a duplicate or
    // out-of-order terminal event must not flip the run to a different
    // terminal stage.
    case "run.cancelled":
      return isSettled(next) ? next : { ...next, stage: "cancelled" };
    case "run.failed":
      return isSettled(next) ? next : { ...next, stage: "failed", reason: event.reason };
    case "run.done":
      return isSettled(next) ? next : { ...next, stage: "done" };
  }
}

/** Ordered by how far through the run each stage is. */
const STAGE_RANK: Record<Stage, number> = {
  idle: 0,
  recall: 1,
  context: 2,
  intent: 3,
  fanout: 4,
  assembling: 5,
  done: 6,
  failed: 6,
  cancelled: 6,
};

/**
 * Move the stage forward, never back — the same one-way rule the inquiry
 * ladder uses, for the same reason. A settled stage is never left.
 */
function advance(current: Stage, to: Stage): Stage {
  if (STAGE_RANK[current] >= STAGE_RANK.done) return current;
  return STAGE_RANK[to] > STAGE_RANK[current] ? to : current;
}

function raise(current: InquiryPhase, to: InquiryPhase): InquiryPhase {
  return RANK[to] > RANK[current] ? to : current;
}

/** A count that only ever grows, so a lost or late event cannot shrink it. */
function grow(current: number | undefined, reported: number | undefined): number | undefined {
  if (reported === undefined) return current;
  return current === undefined ? reported : Math.max(current, reported);
}

/**
 * Apply `update` to inquiry `index`, creating it if this is the first thing
 * ever heard about it, and keep the list dense and ordered.
 *
 * Returns the new state plus a `withExpected` continuation, because the two
 * events that create an inquiry also carry the run's total and it would
 * otherwise take a second spread at each call site.
 */
function withInquiry(
  state: ProgressState,
  index: number,
  update: (one: InquiryProgress) => InquiryProgress,
): { state: ProgressState; withExpected: (n: number | undefined) => ProgressState } {
  const existing = state.inquiries.find((one) => one.index === index);
  const updated = update(existing ?? { index, phase: "planned" });
  const inquiries = existing
    ? state.inquiries.map((one) => (one.index === index ? updated : one))
    : [...state.inquiries, updated].sort((a, b) => a.index - b.index);
  // Seeing an inquiry at all is itself evidence of how many there are, which
  // is what keeps the strip honest when the count-bearing events were lost.
  const next: ProgressState = {
    ...state,
    inquiries,
    expected: grow(state.expected, index + 1),
  };
  return { state: next, withExpected: (n) => ({ ...next, expected: grow(next.expected, n) }) };
}

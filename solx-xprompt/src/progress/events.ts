/**
 * Reading `multi_inquire`'s progress events off the merged console.
 *
 * `solx-inquiry` prints each milestone with a human-readable tagged message
 * *and* a machine-readable `data` object, and since the progress-event work it
 * puts a versioned envelope under the reserved `data.ev` key (see that
 * package's `src/console.rs`). This module turns one console entry into at
 * most one typed event, and never throws: an entry it does not understand is
 * simply not an event.
 *
 * Two rules from the producer's contract that shape everything here:
 *
 * * **Unknown is ignorable.** A new event type, or a new field on an existing
 *   one, keeps `v: 1`. So an unknown `t` is dropped rather than treated as an
 *   error, and unknown fields are left alone. A higher `v` is dropped outright
 *   rather than guessed at.
 * * **Any single event can be lost.** Printing is fire-and-forget on the
 *   producer's side, and a busy shared console can evict a run's early entries
 *   before a widget first tails it. Nothing downstream may require that event
 *   A arrived before event B is understood — see `reducer.ts`.
 */

import type { MergedEntry } from "../console";

export type Scope = "documents" | "actions" | "both";
export type IntentMode = "direct" | "inquire";
export type FanoutMode = "parallel" | "sequential";

export type ProgressEvent =
  | { t: "run.started"; n?: number; model?: string }
  | { t: "run.done"; responses?: number; memories?: number; scripts?: number; errors?: number }
  | { t: "run.failed"; reason?: string }
  | { t: "run.cancelled"; stopped?: number }
  | { t: "recall.done"; skills?: number; memories?: number }
  | { t: "context.done"; docs?: number }
  | { t: "intent.started"; model?: string }
  | { t: "intent.done"; mode: IntentMode; n?: number }
  | { t: "intent.failed"; reason?: string }
  | { t: "inquiry.planned"; i: number; n?: number; kind?: Scope; q?: string }
  | { t: "inquiry.searched"; i: number; hits?: number }
  | { t: "inquiry.started"; i: number; n?: number; kind?: Scope; q?: string }
  | { t: "inquiry.finished"; i: number; status?: string; ok: boolean }
  | { t: "inquiry.failed"; i: number; stage?: string; reason?: string }
  | { t: "fanout.started"; n?: number; mode?: FanoutMode }
  | { t: "fanout.degraded"; n?: number; mode?: FanoutMode; reason?: string };

/** The envelope version this build understands. */
const EV_VERSION = 1;

/**
 * One console entry to at most one typed event.
 *
 * Falls back to [`legacyEventFromMessage`] for an entry with no envelope,
 * which is what keeps a newer widget useful against an older `solx-inquiry`.
 */
export function parseProgressEvent(entry: MergedEntry): ProgressEvent | null {
  const ev = record(entry.data)?.ev;
  const fields = record(ev);
  if (!fields) return legacyEventFromMessage(entry);
  if (num(fields.v) !== EV_VERSION) return null;
  return fromFields(str(fields.t), fields);
}

function fromFields(t: string | undefined, f: Record<string, unknown>): ProgressEvent | null {
  switch (t) {
    case "run.started":
      return { t, n: num(f.n), model: str(f.model) };
    case "run.done":
      return {
        t,
        responses: num(f.responses),
        memories: num(f.memories),
        scripts: num(f.scripts),
        errors: num(f.errors),
      };
    case "run.failed":
      return { t, reason: str(f.reason) };
    case "run.cancelled":
      return { t, stopped: num(f.stopped) };
    case "recall.done":
      return { t, skills: num(f.skills), memories: num(f.memories) };
    case "context.done":
      return { t, docs: num(f.docs) };
    case "intent.started":
      return { t, model: str(f.model) };
    case "intent.done": {
      const mode = str(f.mode);
      // The one required field in the whole vocabulary: an `intent.done` with
      // no mode cannot say whether a fan-out is coming, which is the only
      // thing a consumer reads it for.
      if (mode !== "direct" && mode !== "inquire") return null;
      return { t, mode, n: num(f.n) };
    }
    case "intent.failed":
      return { t, reason: str(f.reason) };
    case "inquiry.planned":
    case "inquiry.started": {
      const i = index(f.i);
      if (i === undefined) return null;
      return { t, i, n: num(f.n), kind: scope(f.kind), q: str(f.q) };
    }
    case "inquiry.searched": {
      const i = index(f.i);
      if (i === undefined) return null;
      return { t, i, hits: num(f.hits) };
    }
    case "inquiry.finished": {
      const i = index(f.i);
      if (i === undefined) return null;
      return { t, i, status: str(f.status), ok: f.ok === true };
    }
    case "inquiry.failed": {
      const i = index(f.i);
      if (i === undefined) return null;
      return { t, i, stage: str(f.stage), reason: str(f.reason) };
    }
    case "fanout.started":
      return { t, n: num(f.n), mode: fanoutMode(f.mode) };
    case "fanout.degraded":
      return { t, n: num(f.n), mode: fanoutMode(f.mode), reason: str(f.reason) };
    // Includes `inquiry.result`, which carries nothing the completion event
    // does not, and anything a newer producer adds.
    default:
      return null;
  }
}

/**
 * `[multi_inquire:...]` tag parsing, for an entry with no `ev` envelope.
 *
 * Not optional polish. The `.wasm` component installs through `install.solx`
 * and this widget ships as a separate bundle, so a widget running ahead of its
 * package is the normal state rather than an edge case — and without this it
 * would show a permanently empty progress strip against one. What it can
 * recover is the counts and the ordering; what it cannot recover is anything
 * that had no pre-envelope print at all, which is every lifecycle edge
 * (`inquiry.started` / `inquiry.finished`), so a legacy run's strip shows
 * inquiries appearing and completing but never a live "running" state.
 *
 * Remove once no installed `solx-inquiry` predates the `data.ev` envelope.
 */
export function legacyEventFromMessage(entry: MergedEntry): ProgressEvent | null {
  const m = /^\[multi_inquire:(?:inquiry:(\d+):)?([a-z]+)\]/.exec(entry.message ?? "");
  if (!m) return null;
  const data = record(entry.data) ?? {};
  const i = m[1] === undefined ? undefined : Number(m[1]);
  const step = m[2];

  if (i !== undefined) {
    switch (step) {
      case "terms":
        return { t: "inquiry.planned", i, kind: scope(data.kind), q: str(data.question) };
      case "hits":
        return { t: "inquiry.searched", i, hits: num(data.count) };
      // Pre-envelope there was no completion event, so the result print is the
      // only evidence an inquiry ever finished.
      case "result":
        return { t: "inquiry.finished", i, status: "ok", ok: true };
      case "error":
        return { t: "inquiry.failed", i, reason: str(data.kind) };
      default:
        return null;
    }
  }

  switch (step) {
    case "intent": {
      const mode = str(data.mode);
      if (mode !== "direct" && mode !== "inquire") return null;
      return { t: "intent.done", mode, n: arrayLength(data.inquiries) };
    }
    case "result":
      return { t: "run.done", responses: num(data.responses), scripts: num(data.scripts) };
    case "recall":
      return { t: "recall.done" };
    case "context":
      return { t: "context.done" };
    default:
      return null;
  }
}

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function str(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

function num(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

/** A usable inquiry index: a non-negative integer, or nothing. */
function index(value: unknown): number | undefined {
  const n = num(value);
  return n !== undefined && Number.isInteger(n) && n >= 0 ? n : undefined;
}

function fanoutMode(value: unknown): FanoutMode | undefined {
  return value === "parallel" || value === "sequential" ? value : undefined;
}

function scope(value: unknown): Scope | undefined {
  return value === "documents" || value === "actions" || value === "both" ? value : undefined;
}

function arrayLength(value: unknown): number | undefined {
  return Array.isArray(value) ? value.length : undefined;
}

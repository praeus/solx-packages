/**
 * The durable half of the merged-console mechanism: what gets persisted so
 * a reload (or another client) can reconstruct the same unified console a
 * live widget session built up.
 *
 * See `../../../docs/widget-system.md` ("hostFromClient: a convenience
 * wrapper...") for why this exists at all: `console-copy` requires an
 * action caller, and a widget's own calls are external execs with no
 * caller, so there is no server-side operation that can merge several
 * actions' consoles into one on the widget's behalf. The workaround is to
 * track *which* (action_ref, invocation_id) pairs belong to one widget
 * session as an ordinary document, and merge their consoles client-side —
 * `console-read`/`console-tail` are unrestricted by caller, so reading is
 * never the blocked half of this.
 */

/** One tracked action invocation — enough to resume tailing it after a reload. */
export interface CallRecord {
  /** The full action reference whose console this invocation wrote to. */
  actionRef: string;
  /** The action's own name, for display when no `label` is given. */
  name: string;
  /**
   * Filters the shared console (one console per `actionRef`, holding every
   * invocation's entries interleaved) down to just this call's own.
   */
  invocationId: string;
  /** The console `seq` in place when this call started; entries before it predate the call. */
  consoleSeqStart: number;
  /** ISO timestamp this call was started. */
  startedAt: string;
  /** Optional display label, shown instead of `name`. */
  label?: string;
}

/** The document contents: every call tracked so far for one widget session, oldest first. */
export interface CallLog {
  calls: CallRecord[];
}

/** One console entry, merged across every tracked call and tagged with where it came from. */
export interface MergedEntry {
  seq: number;
  ts: string;
  level: string;
  actionRef: string;
  invocationId: string;
  label: string;
  message: string | null;
  data: unknown;
}

/** Per-invocation read cursor, carried between calls to `readMergedConsole` so a repeat poll only fetches what's new. */
export type MergeCursors = Record<string, number>;

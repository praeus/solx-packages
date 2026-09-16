/**
 * A unified, reload-safe console view across every action a widget session
 * invokes, without any backend change.
 *
 * `console_copy` — the one server-side primitive that merges consoles —
 * requires an action caller, and a widget's calls are external execs with
 * no caller (see `docs/widget-system.md`), so it can never be used from
 * here. Instead: start each call detached (`client.invocations.start`, which
 * returns an `invocation_id` a plain `exec` never does), persist the
 * (action_ref, invocation_id, cursor) tuple as an ordinary document — the
 * durable index — and merge each call's own console entries client-side.
 * `console_read`/`console_tail` are unrestricted by caller, so this only
 * needed somewhere durable to write down which calls belong together, not
 * any new backend capability.
 */
export { loadCallLog, appendCall, startTrackedCall } from "./callLog";
export { readMergedConsole } from "./merge";
export { CALL_LOG_PATH, CALL_LOG_TYPE } from "./refs";
export type { CallLog, CallRecord, MergedEntry, MergeCursors } from "./types";

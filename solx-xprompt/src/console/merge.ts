import type { WidgetClient } from "../../../solx-widgets/src/shared/widgetClient";
import type { CallLog, MergedEntry, MergeCursors } from "./types";

/**
 * Read every tracked call's console from its recorded (or resumed) cursor,
 * filter each down to just that call's own `invocation_id` — a console is
 * shared by every invocation of its `action_ref`, so without the filter a
 * concurrent, unrelated invocation of the same action would bleed into this
 * merge — and interleave the results into one time-ordered view.
 *
 * Pass back the returned `cursors` on the next call so a repeat poll (e.g.
 * from a UI's own tail loop) only fetches what's new, rather than re-reading
 * each call's full history every time.
 */
export async function readMergedConsole(
  client: WidgetClient,
  log: CallLog,
  cursors: MergeCursors = {},
  opts: { limit?: number; waitSecs?: number } = {},
): Promise<{ entries: MergedEntry[]; cursors: MergeCursors }> {
  const nextCursors: MergeCursors = { ...cursors };

  const perCall = await Promise.all(
    log.calls.map(async (call) => {
      const from = cursors[call.invocationId] ?? call.consoleSeqStart;
      const tail = await client.invocations.tailConsole(call.actionRef, {
        cursor: from,
        limit: opts.limit,
        waitSecs: opts.waitSecs,
      });
      // Advances past every entry read from this action's shared console,
      // not just the ones that were this call's own — correct even when
      // `mine` below is a strict subset of `tail.entries`.
      nextCursors[call.invocationId] = tail.next_cursor ?? from;

      return tail.entries
        .filter((e) => e.invocation_id === call.invocationId)
        .map(
          (e): MergedEntry => ({
            seq: e.seq,
            ts: e.ts,
            level: e.level,
            actionRef: call.actionRef,
            invocationId: call.invocationId,
            label: call.label ?? call.name,
            message: e.message,
            data: e.data,
          }),
        );
    }),
  );

  const entries = perCall.flat().sort((a, b) => (a.ts < b.ts ? -1 : a.ts > b.ts ? 1 : 0));
  return { entries, cursors: nextCursors };
}

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
 *
 * One consequence of that filter worth knowing before wondering where the
 * model's streamed output went: `multi_inquire` drains each child chat call's
 * console into its own with `console-copy`, but copy stamps every copied row
 * with the **source** invocation id (and the source `ts`), so those
 * `[multi_inquire:inquiry:N] <token>` lines are dropped here. What arrives is
 * the milestone prints only. That is what keeps this feed legible — and it is
 * also why the `ts` tie-break below is safe, since a copied entry's timestamp
 * can predate milestones already emitted.
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

  // Ordered by `ts`, then by `seq` within one console. `ts` alone is not
  // enough: it is a server-side `Utc::now()`, and a burst of milestones
  // emitted back to back with no I/O between them genuinely shares one
  // timestamp - three `inquiry.started` events, say. `seq` is the
  // authoritative emission order within a console, so it breaks the tie;
  // `actionRef` only keeps the comparison total across consoles, where `seq`
  // values are unrelated.
  const entries = perCall.flat().sort((a, b) => cmp(a.ts, b.ts) || cmp(a.actionRef, b.actionRef) || a.seq - b.seq);
  return { entries, cursors: nextCursors };
}

function cmp(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

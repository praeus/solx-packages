import type { Host } from "../../../solx-widgets/src/wrap/host";
import type { WidgetClient, WidgetInvocation } from "../../../solx-widgets/src/shared/widgetClient";
import { CALL_LOG_PATH, CALL_LOG_TYPE, GET_DOC, SAVE_DOC } from "./refs";
import type { CallLog, CallRecord } from "./types";

/** Fails open to an empty log — a call log that has never been written to is not an error. */
export async function loadCallLog(host: Host, logId: string): Promise<CallLog> {
  const r = await host.try<{ contents?: CallLog }>(GET_DOC, { path: CALL_LOG_PATH, name: logId });
  if (!r.ok) return { calls: [] };
  return r.value?.contents ?? { calls: [] };
}

/**
 * Read-modify-write, not a real append: `entity-save-document` is a whole-
 * document upsert, and there is no atomic array-append action to reach for
 * instead. Fine for one widget session appending its own calls one at a
 * time (the same assumption solx-agent's session document makes); a second
 * concurrent writer to the *same* `logId` could lose an entry racing this.
 */
export async function appendCall(host: Host, logId: string, record: CallRecord): Promise<CallLog> {
  const log = await loadCallLog(host, logId);
  log.calls = [...log.calls, record];
  await host.call(SAVE_DOC, {
    path: CALL_LOG_PATH,
    name: logId,
    typeRef: CALL_LOG_TYPE,
    title: "xprompt call log " + logId,
    summary: log.calls.length + " call(s) tracked",
    contents: log,
  });
  return log;
}

/**
 * Start an action detached (so it comes back with an `invocation_id` and a
 * `console_seq_start` — a plain `exec` returns neither, see
 * `docs/widget-system.md`), and record it in `logId`'s call log so the
 * merged console this call contributes to survives a reload.
 */
export async function startTrackedCall(
  client: WidgetClient,
  host: Host,
  logId: string,
  path: string,
  name: string,
  params?: unknown,
  label?: string,
): Promise<WidgetInvocation> {
  const inv = await client.invocations.start(path, name, params);
  await appendCall(host, logId, {
    actionRef: inv.action_ref,
    name,
    invocationId: inv.invocation_id,
    consoleSeqStart: inv.console_seq_start,
    startedAt: new Date().toISOString(),
    label,
  });
  return inv;
}

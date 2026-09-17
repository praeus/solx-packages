/**
 * One instruction in, one multi_inquire turn out.
 *
 * multi_inquire's own intent phase decides whether an instruction can be
 * answered directly or needs up to three inquiries fanned out (see
 * solx-inquiry's intent.rs) — one LLM call, schema-constrained, with real
 * context (skills, memories, session history). That is the "chat vs.
 * research" decision, made once and made well; there is no keyword-heuristic
 * routing layer here to duplicate or contradict it.
 *
 * Runs detached via `client.invocations.start` (through this widget's own
 * `startTrackedCall`, see `src/console/`) rather than a blocking `host.call`.
 * That is what gives a turn an `invocation_id` the Composer's Stop button can
 * actually cancel via `client.invocations.stop`, and what lets the Console
 * tab show this turn's recall/intent/inquiry phases as they happen.
 *
 * Action hits multi_inquire returns render with a "Run" button that calls
 * actions.exec with the hit's parameters — the same plumbing the widget's
 * own calls already use, so a user can go from "what actions can do X?" to
 * "run X" without leaving the chat.
 */

import { isTerminalStatus } from "../../solx-widgets/src/shared/widgetClient";
import type { WidgetClient, WidgetInvocation } from "../../solx-widgets/src/shared/widgetClient";
import { compact } from "../../solx-widgets/src/wrap/host";
import type { Host } from "../../solx-widgets/src/wrap/host";
import { startTrackedCall } from "./console";
import { INQUIRY_PATH, MULTI_INQUIRE_FN } from "./refs";
import type { InquireHit, MultiInquireResult } from "./types";

/** How long one long-poll waits for the invocation to change before this loop asks again. */
const POLL_WAIT_SECS = 25;

export interface DispatchHandle {
  invocationId: string;
  /** Resolves once the invocation reaches a terminal state; rejects on failure or cancellation. */
  result: Promise<MultiInquireResult>;
}

/**
 * Start one turn, tracked under `logId` so it shows up in the widget's
 * merged Console tab. Returns as soon as the call is accepted — the caller
 * awaits `.result` separately so it can render the invocation id (for Stop)
 * before the turn finishes.
 */
export async function dispatch(
  client: WidgetClient,
  host: Host,
  logId: string,
  instruction: string,
  model: string,
  session: string,
): Promise<DispatchHandle> {
  const inv = await startTrackedCall(
    client,
    host,
    logId,
    INQUIRY_PATH,
    MULTI_INQUIRE_FN,
    compact({
      instruction,
      model,
      session,
      max_inquiries: 3,
      max_terms: 5,
      max_results: 10,
    }),
    "turn",
  );
  return { invocationId: inv.invocation_id, result: waitForResult(client, inv) };
}

async function waitForResult(client: WidgetClient, inv: WidgetInvocation): Promise<MultiInquireResult> {
  let current = inv;
  while (!isTerminalStatus(current.status)) {
    const polled = await client.invocations.poll(current.invocation_id, POLL_WAIT_SECS);
    if (polled) current = polled;
  }
  if (current.error) {
    throw new Error(current.error);
  }
  return current.result as MultiInquireResult;
}

/**
 * Pull the most plausible parameter object out of an action hit.
 *
 * multi_inquire does not return a hit's full param schema inline — it's
 * fetched separately and stuffed under `hit.details.paramSchema`. We don't
 * have it here at the dispatch layer (this module is pure plumbing), so the
 * caller builds the params object from the schema at the call site. This
 * helper just decides which action hit is runnable: one whose source is
 * "action" and whose details carry a category so the user can see what kind
 * of action it is.
 */
export function isRunnableActionHit(hit: InquireHit): boolean {
  return hit.source === "action" && !!hit.details?.category;
}

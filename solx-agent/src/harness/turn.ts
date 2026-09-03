/**
 * One iteration: ask the model, gate what it asked for, dispatch, and flush
 * the results together.
 *
 * This is a plain tool-calling loop -- there is no planner, no evaluator, no
 * reflection step, and **no base system prompt at all**. The model sees only
 * the operator's preamble, the memory/context/skill blocks, the conversation
 * and the tool definitions. That makes the preamble the entire behavioural
 * contract, which is why the widget exposes it as an editable field rather
 * than hiding it in the bundle.
 *
 * ## Durability
 *
 * The conductor got atomicity for free: one wasm invocation did load -> chat
 * -> dispatch -> save, and a closed browser tab could not interrupt it.
 * Driving from the browser gives that up, so this file buys it back by
 * persisting *more* often rather than less -- after the model replies, after
 * every individual dispatched call, and again at flush.
 *
 * The result is better than what it replaced. A crashed step used to lose a
 * whole iteration including tool calls that had already committed their side
 * effects; now the document always describes what actually happened, and
 * `completeTurn` resumes from it because a half-filled `pending[]` is exactly
 * the shape it already knew how to finish.
 */

import { permitted, reservedDocWrite, splitRef, isExecutableType } from "./gate";
import { callId, saveSession } from "./session";
import { isSysTool, runSysTool } from "./sysTools";
import { CHAT, GET_ACTION, MAX_CONSECUTIVE_FAILURES } from "./refs";
import type { Host } from "./host";
import type { Message, PendingCall, PendingView, Session, StepResult, ToolCall } from "./types";

/**
 * Everything a call must survive, decided at the moment of dispatch rather
 * than trusted from the session document.
 *
 * That matters because the session *is* a document: a model that can write
 * documents at all is one bug away from editing its own `tools` map or its
 * own `destructive` flags. Re-running the gate here makes those fields
 * non-authoritative, so tampering with them buys nothing.
 *
 * Destructive-ness is the same union solx-mcp uses: config rules union the
 * row's `solx:destructive` capability union every Command and Webhook row,
 * unconditionally. It fails **closed** -- an action that cannot be read back
 * is treated as destructive rather than waved through.
 */
export async function gateCall(
  host: Host,
  session: Session,
  ref: string,
  args: Record<string, unknown>,
): Promise<{ allowed: boolean; destructive: boolean; reason: string | null }> {
  const parts = splitRef(ref);
  const r = await host.try<{
    path: string;
    name: string;
    capabilities?: string[];
    actionType?: string;
    action_type?: string;
  }>(GET_ACTION, { path: parts.path, name: parts.name, excludeHidden: true });

  if (!r.ok || !r.value) {
    return { allowed: false, destructive: true, reason: "action no longer available" };
  }
  const a = r.value;
  if (!permitted(a, session.grant)) {
    return {
      allowed: false,
      destructive: true,
      reason: "refused: " + ref + " is not permitted in this session",
    };
  }
  const reserved = reservedDocWrite(session, ref, args);
  if (reserved) return { allowed: false, destructive: true, reason: reserved };

  const caps = a.capabilities || [];
  const ty = a.actionType || a.action_type;
  return {
    allowed: true,
    destructive: caps.indexOf("solx:destructive") !== -1 || isExecutableType(ty),
    reason: null,
  };
}

async function chat(host: Host, session: Session): Promise<Partial<Message> & { tool_calls?: ToolCall[] }> {
  const out = await host.call<{ message?: Record<string, unknown> }>(CHAT, {
    model: session.model,
    messages: session.messages,
    tools: session.tools_defs,
    timeout_secs: session.chat_timeout_secs || null,
  });
  return ((out && out.message) || {}) as Partial<Message> & { tool_calls?: ToolCall[] };
}

/**
 * Every entry in `message.tool_calls` needs exactly one `role:"tool"` turn in
 * reply -- denials included. A model that gets no answer for a call it made
 * will usually just re-issue it. `transcript.ts` depends on this too: it
 * pairs results to calls by position.
 */
function toolTurn(name: string, content: string): Message {
  return { role: "tool", tool_name: name, content: String(content) };
}

/** What still needs a decision, and the only place the operator sees the
 *  arguments they are approving. */
export function pendingView(session: Session): PendingView[] {
  return (session.pending || [])
    .filter((p) => p.result === null && p.destructive)
    .map((p) => ({ call_id: p.call_id, name: p.name, ref: p.ref, arguments: p.arguments }));
}

export function summarize(session: Session, status: Session["status"]): StepResult {
  return {
    session_id: session.id,
    status,
    iteration: session.iteration,
    turn_iteration: session.turn_iteration,
    max_iterations: session.max_iterations,
    answer: session.answer || null,
    pending: pendingView(session),
    tools: Object.keys(session.tools || {}),
    tools_dropped: session.tools_dropped || 0,
    memories_written: session.memories_written || 0,
  };
}

/**
 * Execute what may be executed, suspend if anything still needs a human, and
 * flush the whole turn's tool results together once nothing is outstanding.
 *
 * Results are held rather than appended as they land, so the transcript never
 * shows half a turn to the model. They *are* persisted as they land, which is
 * a different thing -- see this module's Durability note.
 */
export async function completeTurn(
  host: Host,
  session: Session,
  approved: string[],
): Promise<StepResult> {
  const pending = session.pending || [];
  let awaiting = false;

  for (const p of pending) {
    if (p.result !== null) continue;

    if (p.refusal) {
      p.result = { outcome: "refused", content: p.refusal };
      await saveSession(host, session.id, session);
      continue;
    }
    if (p.sys) {
      p.result = await runSysTool(host, session, p.name, p.arguments);
      await saveSession(host, session.id, session);
      continue;
    }

    const gate = await gateCall(host, session, p.ref as string, p.arguments);
    if (!gate.allowed) {
      p.result = { outcome: "refused", content: gate.reason as string };
      await saveSession(host, session.id, session);
      continue;
    }
    p.destructive = gate.destructive;

    if (p.destructive && approved.indexOf(p.call_id) === -1) {
      // Not yet approved. Anything not named in `approve` on the *resuming*
      // call is a denial; on the first pass it is simply outstanding.
      if (session.status === "awaiting_approval") {
        p.result = { outcome: "denied", content: "denied by operator" };
        await saveSession(host, session.id, session);
        continue;
      }
      awaiting = true;
      continue;
    }

    const r = await host.try(p.ref as string, p.arguments);
    p.result = r.ok
      ? { outcome: "ok", content: JSON.stringify(r.value) }
      : { outcome: "error", content: "error: " + r.error };
    p.approved_by = p.destructive ? "operator" : null;
    // Persist immediately: this call may have just changed the world, and a
    // tab closed on the next one must not lose the record of it.
    await saveSession(host, session.id, session);
  }

  if (awaiting) {
    session.status = "awaiting_approval";
    await saveSession(host, session.id, session);
    return summarize(session, "awaiting_approval");
  }

  // Flush: one tool turn per call, in call order, then clear pending so no
  // approval can survive into the next iteration.
  let failures = 0;
  for (const p of pending) {
    const result = p.result as { outcome: PendingCall["result"] extends null ? never : string; content: string };
    session.messages.push(toolTurn(p.name, result.content));
    session.calls.push({
      iteration: session.iteration,
      name: p.name,
      ref: p.ref,
      outcome: p.result!.outcome,
      approved_by: p.approved_by || null,
    });
    if (p.result!.outcome !== "ok") failures++;
  }
  const allFailed = pending.length > 0 && failures === pending.length;
  session.consecutive_failures = allFailed ? (session.consecutive_failures || 0) + 1 : 0;
  session.pending = [];
  session.status =
    session.consecutive_failures >= MAX_CONSECUTIVE_FAILURES ? "blocked" : "running";

  await saveSession(host, session.id, session);
  return summarize(session, session.status);
}

/**
 * Run exactly one iteration.
 *
 * `approve` resolves a suspended turn: anything not named is denied, so an
 * empty or missing list denies everything, which is the safe direction to
 * fail.
 */
export async function step(
  host: Host,
  session: Session,
  approve?: string[],
): Promise<StepResult> {
  if (session.status !== "running" && session.status !== "awaiting_approval") {
    return summarize(session, session.status);
  }
  const approved = Array.isArray(approve) ? approve : [];

  // Resuming a suspended turn: the model was already asked, and `pending`
  // holds what it wanted. Nothing new is sent until the turn completes. This
  // must come before the cap check below -- that iteration was already spent
  // asking the model, so resuming it must not be mistaken for starting a new
  // one that the cap would then refuse.
  if (session.status === "awaiting_approval") {
    return completeTurn(host, session, approved);
  }

  // A turn that crashed mid-dispatch left results half-filled. Finishing it
  // is the same operation as resuming an approval, and it must happen before
  // anything new is asked of the model.
  if ((session.pending || []).some((p) => p.result === null)) {
    return completeTurn(host, session, approved);
  }

  // The budget is per *turn*, not per session: `iteration` stays monotonic as
  // the audit trail, and every turn gets the same allowance regardless of
  // what earlier ones spent.
  if (session.turn_iteration >= session.max_iterations) {
    session.status = "exhausted";
    await saveSession(host, session.id, session);
    return summarize(session, "exhausted");
  }

  session.iteration++;
  session.turn_iteration++;

  const message = await chat(host, session);
  const calls = Array.isArray(message.tool_calls) ? message.tool_calls : [];

  // The assistant turn goes in verbatim -- including its tool_calls, which
  // the model needs to see echoed back alongside the results.
  session.messages.push({
    role: "assistant",
    content: (message as { content?: string }).content || "",
    ...(calls.length > 0 ? { tool_calls: calls } : {}),
  } as Message);

  if (calls.length === 0) {
    // The model replied without calling a tool. That covers "the task is
    // done" *and* "which of these did you mean?", and this deliberately does
    // not try to tell them apart: the model already signals structurally, and
    // guessing from prose would be exactly the parsing this package avoids
    // everywhere else. Either way the next thing is a human, so it is the
    // user's turn -- which is what `idle` says.
    session.status = "idle";
    session.answer = (message as { content?: string }).content || "";
    await saveSession(host, session.id, session);
    return summarize(session, "idle");
  }

  session.pending = calls.map((c): PendingCall => {
    const fn = c.function || ({} as ToolCall["function"]);
    const toolName = fn.name || "";
    const args = fn.arguments || {};
    if (isSysTool(toolName)) {
      return {
        call_id: callId(toolName, args),
        name: toolName,
        ref: null,
        arguments: args,
        sys: true,
        refusal: null,
        destructive: false,
        result: null,
      };
    }
    const ref = session.tools[toolName] || null;
    return {
      call_id: callId(toolName, args),
      name: toolName,
      ref,
      arguments: args,
      sys: false,
      // An unlisted name is refused here rather than at dispatch: the map is
      // the only dispatch table, so a name absent from it names nothing.
      refusal: ref === null ? "unknown tool '" + toolName + "'" : null,
      // Provisional only. gateCall re-decides this at dispatch, so nothing
      // downstream trusts what was written into the document here.
      destructive: false,
      result: null,
    };
  });

  // Persist the model's reply and what it asked for *before* dispatching any
  // of it, so a tab closed mid-dispatch leaves a document that says what was
  // in flight.
  await saveSession(host, session.id, session);

  return completeTurn(host, session, approved);
}

/**
 * One model call, driven detached when the host allows it.
 *
 * Promoted out of `solx-agent` (originally a port of `solx-prompt/crate/src/
 * llm.rs`'s detached loop) once a second widget wanted the same thing —
 * `solx-inquiry`'s pipeline, which needs exactly this start/poll/stop shape
 * with a *different* payload. That is the same reason `host.ts` was promoted,
 * and this file follows its conventions: a widget imports it by relative path
 * and re-exports or wraps it, and only the wrapper knows what shape the
 * payload is.
 *
 * ## Why detached at all
 *
 * A blocking `exec` cannot be interrupted from outside. Two things fall out of
 * that which both consumers need:
 *
 * - **Cancellation.** A detached call is owned by the host, so this loop can
 *   check `isAbandoned` between polls and `action-stop` the child. A blocking
 *   call could only be waited on.
 * - **Streaming.** The child streams into its own console, which the host owns
 *   and can drain. Not consumed here — both widgets render from their session
 *   document, not the console — but the detached shape is what makes it
 *   possible without a second chat implementation.
 *
 * It tries `action-start` first and falls back to a plain blocking call when
 * the host refuses: under a bare `solx exec` there is no long-lived process to
 * hold a detached child, so `action-start` declines and cancellation is simply
 * unavailable there.
 *
 * ## Why the request is generic
 *
 * The payload is the caller's business, not this module's. `solx-agent` needs
 * a tool catalogue on every call; `solx-inquiry`'s planner needs `format` (a
 * JSON Schema) and no tools, and its writer needs neither. Modelling that as a
 * union of session types here would only make each caller lie about its own
 * shape, so `ChatRequest` is a small superset and a caller fills in what it
 * has.
 */

import type { Host } from "./host";

/**
 * One message in a chat request or reply.
 *
 * Deliberately permissive: `role` and `content` are what both callers always
 * set, and `tool_calls` is `unknown` because the two use that field
 * differently — the agent narrows it to its own `ToolCall[]`, inquiry never
 * sets it. A strict union here would force one of them to cast.
 */
export interface ChatMessage {
  role: string;
  content: string;
  tool_calls?: unknown;
}

/** What a reply looks like before a caller narrows it. */
export interface ChatResult {
  role?: string;
  content?: string;
  tool_calls?: unknown;
}

export interface ChatRequest {
  /**
   * The chat action to call. Defaults to ollama-chat, which is what both
   * current callers want; a caller pointing at a different chat-completion
   * action names it here rather than this module growing a parameter.
   */
  action?: string;
  model?: string;
  messages: ChatMessage[];
  /** Structured-output schema, or the string "json". Omitted for free text. */
  format?: unknown;
  /** Tool definitions, for a caller whose model picks tools. */
  tools?: unknown;
  timeout_secs?: number | null;
}

export const ACTION_START = "/builtin/action/start";
export const ACTION_STOP = "/builtin/action/stop";
export const ACTION_POLL = "/builtin/action/poll";
export const ACTION_CANCELLED = "/builtin/action/cancelled";

/** The default chat action — `solx-ollama`'s chat endpoint. */
export const DEFAULT_CHAT_ACTION = "/packages/solx-ollama/ollama-chat";

/** How long one `action-poll` long-polls before this loop wakes to re-check
 *  abandonment. Mirrors `solx-prompt`'s `POLL_WAIT_SECS`. Bounds Stop latency:
 *  a Stop lands within one slice, not mid-token. */
const POLL_WAIT_SECS = 5;

/** Substring of `action-start`'s refusal under a bare `solx exec` — see
 *  `solx-actions/src/lib.rs::start_invocation`. Detecting by text is fragile
 *  but there is no error code on the exec boundary to switch on instead. */
const LONG_LIVED_HOST_MARKER = "long-lived host";

const TERMINAL = ["ok", "failed", "cancelled", "timeout", "interrupted"];

/** `true` once a status can never change again. Mirrors
 *  `solx-actions`' `invocations::is_terminal`. */
export function isTerminalStatus(status: string): boolean {
  return TERMINAL.includes(status);
}

/** Thrown when the caller abandoned the run mid-call, so the driving loop can
 *  tell a Stop from a real failure. Exported so a caller's `catch` can compare
 *  against the same class object this throws. */
export class ChatCancelledError extends Error {
  constructor() {
    super("chat cancelled");
    this.name = "ChatCancelledError";
  }
}

/** Split an action ref into its `(path, name)` halves for `action-start`.
 *  Inlined rather than imported, so this module depends on nothing but the
 *  `Host` seam. */
function splitRef(ref: string): { path: string; name: string } {
  const slash = ref.lastIndexOf("/");
  return { path: ref.slice(0, slash) || "/", name: ref.slice(slash + 1) };
}

/** The chat payload, identical for the detached and blocking paths. */
function payloadFor(request: ChatRequest): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    messages: request.messages,
    timeout_secs: request.timeout_secs || null,
  };
  if (request.model !== undefined) payload.model = request.model;
  // Only attach what was asked for. `format: null` is an explicit null and the
  // schema is validated, so an empty field is an error rather than "absent";
  // and a caller that sends no tools must not send an empty `tools` array by
  // accident, because a model without tool support rejects the request.
  if (request.format !== undefined && request.format !== null) payload.format = request.format;
  if (request.tools !== undefined && request.tools !== null) payload.tools = request.tools;
  return payload;
}

/**
 * Ask the model, detached when possible.
 *
 * `isAbandoned` is checked between polls; when it reports true the child is
 * `action-stop`ped and a [`ChatCancelledError`] is thrown so the driving loop
 * can distinguish a Stop from a real failure. Passing none never cancels.
 */
export async function chat(
  host: Host,
  request: ChatRequest,
  isAbandoned?: () => boolean,
): Promise<ChatResult> {
  const action = request.action || DEFAULT_CHAT_ACTION;
  const { path, name } = splitRef(action);
  let start: { invocation_id?: string };
  try {
    start = await host.call<{ invocation_id?: string }>(ACTION_START, {
      path,
      name,
      params: payloadFor(request),
    });
  } catch (e) {
    const message = String((e as Error)?.message ?? e);
    if (message.includes(LONG_LIVED_HOST_MARKER)) {
      return chatBlocking(host, action, request);
    }
    throw e;
  }

  if (!start.invocation_id) throw new Error("action-start returned no invocation_id");
  return pollToCompletion(host, start.invocation_id, isAbandoned);
}

/** The plain single blocking call, for a host that cannot detach. */
async function chatBlocking(host: Host, action: string, request: ChatRequest): Promise<ChatResult> {
  const out = await host.call<{ message?: Record<string, unknown> }>(action, payloadFor(request));
  return ((out && out.message) || {}) as unknown as ChatResult;
}

async function pollToCompletion(
  host: Host,
  invocationId: string,
  isAbandoned?: () => boolean,
): Promise<ChatResult> {
  // eslint-disable-next-line no-constant-condition
  while (true) {
    if (isAbandoned?.()) {
      await stopChild(host, invocationId);
      throw new ChatCancelledError();
    }
    const poll = await host.call<{ status?: string; result?: unknown; error?: string }>(
      ACTION_POLL,
      { invocation_id: invocationId, wait_secs: POLL_WAIT_SECS },
    );
    const status = poll.status || "";
    if (isTerminalStatus(status)) return finish(status, poll);
  }
}

async function stopChild(host: Host, invocationId: string): Promise<void> {
  try {
    await host.call(ACTION_STOP, { invocation_id: invocationId });
  } catch {
    /* best-effort: the host force-aborts an un-stopped child after its grace */
  }
}

function finish(
  status: string,
  poll: { status?: string; result?: unknown; error?: string },
): ChatResult {
  if (status === "ok") {
    const result = poll.result as { message?: Record<string, unknown> } | undefined;
    return ((result && result.message) || {}) as unknown as ChatResult;
  }
  throw new Error(
    "chat did not complete (status: " + status + "): " + (poll.error || "no message"),
  );
}

/** The text of a reply, regardless of which field the model used. */
export function messageText(message: ChatResult | null | undefined): string {
  if (!message) return "";
  return typeof message.content === "string" ? message.content : "";
}

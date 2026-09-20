/**
 * One model call, driven detached when the host allows it.
 *
 * The harness used to call `ollama-chat` with a single blocking `exec`. That
 * works, but two things fall out of it that a headless agent needs and the UI
 * wants even now:
 *
 * - **Cancellation.** A blocking call cannot be interrupted from outside. A
 *   detached one can: the host owns the child, and this loop checks
 *   `isAbandoned` (the in-page Stop signal; the headless port will instead
 *   check `action-cancelled`) between polls and `action-stop`s the child.
 * - **Streaming.** The child streams into its own console; a later drain
 *   mirrors that into the caller's. Not wired here yet — the in-page UI
 *   renders from the session document, not the console — but the detached
 *   shape is what makes it possible without a second chat implementation.
 *
 * This is a port of `solx-prompt/crate/src/llm.rs`'s detached loop, which is
 * the one piece of the planner lineage worth keeping. It tries
 * `action-start` first and falls back to a plain blocking call when the host
 * refuses — under a bare `solx exec` there is no long-lived process to hold
 * the detached child, so `action-start` declines and cancellation is simply
 * unavailable there.
 */

import { splitRef } from "./gate";
import { CHAT } from "./refs";
import type { Host } from "./host";
import type { Message, Session, ToolCall } from "./types";

export const ACTION_START = "/builtin/action/start";
export const ACTION_STOP = "/builtin/action/stop";
export const ACTION_POLL = "/builtin/action/poll";
export const ACTION_CANCELLED = "/builtin/action/cancelled";

/** How long one `action-poll` long-polls before this loop wakes to re-check
 *  abandonment. Mirrors `solx-prompt`'s `POLL_WAIT_SECS`. Bounds Stop latency:
 *  a Stop lands within one slice, not mid-token. */
const POLL_WAIT_SECS = 5;

/** Substring of `action-start`'s refusal under a bare `solx exec` — see
 *  `solx-actions/src/lib.rs::start_invocation`. Detecting by text is fragile
 *  but there is no error code on the exec boundary to switch on instead. */
const LONG_LIVED_HOST_MARKER = "long-lived host";

const TERMINAL_OK = "ok";
const TERMINAL_FAILED = "failed";
const TERMINAL_CANCELLED = "cancelled";
const TERMINAL_TIMEOUT = "timeout";
const TERMINAL_INTERRUPTED = "interrupted";

function isTerminal(status: string): boolean {
  return (
    status === TERMINAL_OK ||
    status === TERMINAL_FAILED ||
    status === TERMINAL_CANCELLED ||
    status === TERMINAL_TIMEOUT ||
    status === TERMINAL_INTERRUPTED
  );
}

/** Thrown when the operator abandoned the run mid-call. `driveSession` turns
 *  this back into a graceful stop rather than an error banner. */
export class ChatCancelledError extends Error {
  constructor() {
    super("chat cancelled");
    this.name = "ChatCancelledError";
  }
}

export type ChatMessage = Partial<Message> & { tool_calls?: ToolCall[] };

/** The chat payload, identical for the detached and blocking paths. */
function payloadFor(session: Session): Record<string, unknown> {
  return {
    model: session.model,
    messages: session.messages,
    tools: session.tools_defs,
    timeout_secs: session.chat_timeout_secs || null,
  };
}

/**
 * Ask the model for the next step, detached when possible.
 *
 * `isAbandoned` is checked between polls; when it reports true the child is
 * `action-stop`ped and a [`ChatCancelledError`] is thrown so the driving loop
 * can distinguish a Stop from a real failure. Passing none (the tests do)
 * never cancels.
 */
export async function chat(
  host: Host,
  session: Session,
  isAbandoned?: () => boolean,
): Promise<ChatMessage> {
  const { path, name } = splitRef(CHAT);
  let start: { invocation_id?: string };
  try {
    start = await host.call<{ invocation_id?: string }>(ACTION_START, {
      path,
      name,
      params: payloadFor(session),
    });
  } catch (e) {
    const message = String((e as Error)?.message ?? e);
    if (message.includes(LONG_LIVED_HOST_MARKER)) {
      return chatBlocking(host, session);
    }
    throw e;
  }

  if (!start.invocation_id) {
    throw new Error("action-start returned no invocation_id");
  }
  return pollToCompletion(host, start.invocation_id, isAbandoned);
}

/** The plain single blocking call, for a host that cannot detach. */
async function chatBlocking(host: Host, session: Session): Promise<ChatMessage> {
  const out = await host.call<{ message?: Record<string, unknown> }>(CHAT, payloadFor(session));
  return ((out && out.message) || {}) as ChatMessage;
}

async function pollToCompletion(
  host: Host,
  invocationId: string,
  isAbandoned?: () => boolean,
): Promise<ChatMessage> {
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
    if (isTerminal(status)) {
      return finish(status, poll);
    }
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
): ChatMessage {
  if (status === TERMINAL_OK) {
    const result = poll.result as { message?: Record<string, unknown> } | undefined;
    return ((result && result.message) || {}) as ChatMessage;
  }
  throw new Error(
    "chat did not complete (status: " + status + "): " + (poll.error || "no message"),
  );
}

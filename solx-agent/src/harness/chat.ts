/**
 * The agent's chat call: the shared detached loop in `solx-widgets`, bound to
 * a session's payload.
 *
 * This file used to be the implementation. It was promoted — start/poll/stop,
 * the blocking fallback, the terminal-status handling and message extraction
 * all moved to `solx-widgets/src/wrap/chat.ts` — once `solx-inquiry` needed the
 * same thing for a different payload, exactly as `host.ts` was promoted
 * before it. What is left here is the part that is genuinely this harness's:
 * knowing that an agent's chat call sends `session.messages`,
 * `session.tools_defs` and `session.model`.
 *
 * The public surface is unchanged on purpose — `turn.ts` imports `chat`,
 * `ChatCancelledError` and `ChatMessage` from `./chat` and does not need to
 * know any of this happened. The action refs are re-exported too, because
 * `HARD_DENY` in `refs.ts` denies those four names to the model and the
 * coupling reads better from one place.
 */

import {
  chat as sharedChat,
  type ChatResult,
} from "../../../solx-widgets/src/wrap/chat";
import { CHAT } from "./refs";
import type { Host } from "./host";
import type { Message, Session, ToolCall } from "./types";

export {
  ACTION_CANCELLED,
  ACTION_POLL,
  ACTION_START,
  ACTION_STOP,
  ChatCancelledError,
} from "../../../solx-widgets/src/wrap/chat";

/**
 * The agent's view of a reply: a partial message, with `tool_calls` narrowed
 * to the harness's own `ToolCall`. The shared module leaves the field
 * `unknown` because the two callers use it differently; the narrowing belongs
 * here, where the tool loop that consumes it lives.
 */
export type ChatMessage = Partial<Message> & { tool_calls?: ToolCall[] };

/** The chat payload. Identical for the detached and blocking paths, which is
 *  now the shared module's concern; this only says what an agent sends. */
function payloadFor(session: Session) {
  return {
    action: CHAT,
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
 * can distinguish a Stop from a real failure. Passing none never cancels.
 */
export async function chat(
  host: Host,
  session: Session,
  isAbandoned?: () => boolean,
): Promise<ChatMessage> {
  const reply: ChatResult = await sharedChat(host, payloadFor(session), isAbandoned);
  return reply as unknown as ChatMessage;
}

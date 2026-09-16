/**
 * One prompt in, one of two paths out:
 *
 *   - chat:      user → ollama-chat                 → text reply
 *   - inquire:   user → inquire (scope: both)        → summary + hits
 *
 * Routing is heuristic — research triggers in the prompt send it to
 * inquire, anything else to chat. A `/research foo` slash-prefix is an
 * explicit opt-in to inquire, regardless of trigger words, so a user can
 * force research without rephrasing.
 *
 * Action hits surfaced by inquire render with a "Run" button that calls
 * actions.exec with the hit's parameters — the same plumbing the widget's
 * own calls already use, so a user can go from "what actions can do X?"
 * to "run X" without leaving the chat.
 */

import { compact } from "../../solx-widgets/src/wrap/host";
import type { Host } from "../../solx-widgets/src/wrap/host";
import { INQUIRE, OLLAMA_CHAT, RESEARCH_TRIGGERS } from "./refs";
import type { InquireHit, InquireResult, OllamaChatResult } from "./types";

export type DispatchResult =
  | { kind: "chat"; text: string; model: string }
  | { kind: "inquire"; result: InquireResult; model: string };

export function isResearch(prompt: string): boolean {
  const trimmed = prompt.trim();
  if (trimmed.startsWith("/research ") || trimmed === "/research") return true;
  const lower = trimmed.toLowerCase();
  return RESEARCH_TRIGGERS.some((t) => lower.includes(t));
}

/** Strip a `/research ` prefix so the prompt that reaches inquire is the natural-language one. */
function stripResearchPrefix(prompt: string): string {
  const t = prompt.trim();
  if (t.startsWith("/research ")) return t.slice("/research ".length);
  if (t === "/research") return "";
  return t;
}

/**
 * Run a single conversational turn. Throws on action failure — the caller
 * renders the failure as an error turn, the way the merged-console
 * mechanism renders any other failure. Uses `host.call` because a chat
 * failure means the surrounding turn cannot continue.
 */
export async function dispatchChat(
  host: Host,
  prompt: string,
  model: string,
): Promise<DispatchResult> {
  const result = await host.call<OllamaChatResult>(OLLAMA_CHAT, compact({
    model,
    messages: [
      { role: "user", content: prompt },
    ],
  }));
  const text = (result?.message?.content ?? "").toString();
  return { kind: "chat", text, model };
}

/**
 * Run a research turn. inquire returns a structured result with both the
 * natural-language summary and the raw hits — the widget renders the
 * summary as the reply and the hits (when actions) as executable
 * suggestions inline.
 */
export async function dispatchInquire(
  host: Host,
  prompt: string,
  model: string,
  scope: "documents" | "actions" | "both" = "both",
): Promise<DispatchResult> {
  const inquiry = stripResearchPrefix(prompt);
  const result = await host.call<InquireResult>(INQUIRE, compact({
    inquiry,
    model,
    scope,
    max_terms: 5,
    max_results: 10,
  }));
  return { kind: "inquire", result, model };
}

/** Top-level entry: route then dispatch. */
export async function dispatch(
  host: Host,
  prompt: string,
  model: string,
): Promise<DispatchResult> {
  return isResearch(prompt)
    ? dispatchInquire(host, prompt, model)
    : dispatchChat(host, prompt, model);
}

/**
 * Pull the most plausible parameter object out of an action hit.
 *
 * `search-actions` does not return a hit's full param schema by itself —
 * inquire fetches it separately and stuffs it under `hit.details.paramSchema`.
 * We don't have it here at the dispatch layer (this module is pure
 * plumbing), so the caller builds the params object from the schema at the
 * call site. This helper just decides which action-hit is runnable: one
 * whose source is "action" and whose details carry a category so the user
 * can see what kind of action it is.
 */
export function isRunnableActionHit(hit: InquireHit): boolean {
  return hit.source === "action" && !!hit.details?.category;
}

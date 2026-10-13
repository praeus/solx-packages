/**
 * Sessions: creating one, and asking it a follow-up.
 *
 * Much smaller than solx-agent's `agent.ts`, because the only thing that
 * differs between turn one and turn five here is whether a document already
 * exists. There is no catalogue to resolve, no grant to normalise, no
 * memory/skills/context to seed -- the entire per-session state is the model,
 * the options, and the turn list.
 */

import { newInquiryId, saveInquiry } from "./session";
import { normalizeOptions, DEFAULT_OPTIONS } from "./options";
import type { Host } from "./host";
import type { InquiryOptions, InquirySession } from "./types";

export interface StartOptions {
  model: string;
  options?: Partial<InquiryOptions> | null;
  /** Optional title override; the first question is the default. */
  title?: string | null;
}

function titleFrom(question: string): string {
  return question.length > 80 ? question.slice(0, 79) + "…" : question;
}

/**
 * Create the inquiry document around its first question. Nothing runs here --
 * `runInquiry` does that, so a caller can create, inspect and then run.
 */
export async function createInquiry(
  host: Host,
  question: string,
  opts: StartOptions,
): Promise<InquirySession> {
  if (!opts.model) throw new Error("a model is required");
  if (!question) throw new Error("a question is required");

  const id = await newInquiryId(host);
  const session: InquirySession = {
    id,
    model: opts.model,
    title: opts.title || titleFrom(question),
    // Not `planning`: nothing is running yet, and a stored live status is only
    // true while `runInquiry` is executing. A session created and never run
    // (a plan preview, headless `plan_only`) must not read as in flight.
    status: "idle",
    options: normalizeOptions({ ...DEFAULT_OPTIONS, ...(opts.options || {}) }),
    turns: [],
    answer: null,
  };
  await saveInquiry(host, id, session);
  return session;
}

/**
 * Apply a follow-up's option overrides to an existing session without
 * rebuilding it -- so the model and the history stay, and only the budgets
 * change. A session edited from the widget's options panel lands here.
 */
export function applyOptions(session: InquirySession, patch: Partial<InquiryOptions> | null): void {
  if (!patch) return;
  session.options = normalizeOptions({ ...session.options, ...patch });
}

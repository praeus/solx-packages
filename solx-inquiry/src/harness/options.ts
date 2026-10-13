/**
 * Options: what an inquiry may be tuned to do, with the hard caps applied.
 *
 * Kept separate from `pipeline.ts` so the same normalisation is used when a
 * session is created and when an existing one is run -- a session document
 * written by an older version, or edited by hand, still gets clamped rather
 * than trusted. Nothing here is a security boundary: there is nothing to
 * fence. It is a budget, and budgets fail soft.
 */

import { clamp } from "./text";
import {
  DEFAULT_DOC_BUDGET,
  DEFAULT_MAX_DOCS,
  DEFAULT_MAX_PLAN_STEPS,
  MAX_DOC_BUDGET,
  MAX_DOCS_HARD,
  MAX_PLAN_STEPS_HARD,
} from "./refs";
import type { InquiryOptions } from "./types";

export const DEFAULT_OPTIONS: InquiryOptions = {
  max_docs: DEFAULT_MAX_DOCS,
  max_plan_steps: DEFAULT_MAX_PLAN_STEPS,
  doc_budget: DEFAULT_DOC_BUDGET,
  include_actions: false,
  replan: true,
  chat_timeout_secs: null,
};

/** Coerce loose input into a valid, capped options object. */
export function normalizeOptions(input: Partial<InquiryOptions> | null | undefined): InquiryOptions {
  const o = input || {};
  const timeout = typeof o.chat_timeout_secs === "number" && isFinite(o.chat_timeout_secs)
    ? Math.max(1, Math.floor(o.chat_timeout_secs))
    : null;
  return {
    max_docs: clamp(o.max_docs ?? DEFAULT_MAX_DOCS, 1, MAX_DOCS_HARD),
    max_plan_steps: clamp(o.max_plan_steps ?? DEFAULT_MAX_PLAN_STEPS, 1, MAX_PLAN_STEPS_HARD),
    doc_budget: clamp(o.doc_budget ?? DEFAULT_DOC_BUDGET, 1000, MAX_DOC_BUDGET),
    include_actions: o.include_actions === true,
    replan: o.replan !== false,
    chat_timeout_secs: timeout,
  };
}

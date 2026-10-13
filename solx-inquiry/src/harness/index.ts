/**
 * The inquiry harness: ask a question, get an answer grounded in solx
 * documents, running in the page rather than behind an action.
 *
 * Where solx-agent hands a model a tool catalogue and lets it drive, this
 * hands a model five read-only verbs and drives the rest itself. Three phases
 * -- plan, gather, answer -- with one bounded retry, and no approval, because
 * every verb is a read. That is the whole design, and this module map is
 * correspondingly short.
 *
 * Everything it knows lives in the document store under `/inquiry`, so an
 * inquiry is inspected, searched and continued with the tools solx already
 * has. There is no private storage anywhere in this package.
 *
 * Module map, roughly in dependency order:
 *
 * - `host`      the single `exec` seam everything below talks through
 * - `refs`      the fixed verb set, the builtins it maps to, budget constants
 * - `text`      truncation, clamping, hashing
 * - `chat`      one model call, detached when the host allows it (shared, from solx-widgets)
 * - `plan`      the plan schema, and validation of what the planner returned
 * - `options`   the tuning budgets, clamped
 * - `corpus`    the descriptive index the planner is shown
 * - `execute`   running a plan deterministically, and gathering documents
 * - `prompt`    the two briefs, and history/document rendering
 * - `session`   the inquiry document, and its name
 * - `agent`     creating a session and applying follow-up options
 * - `pipeline`  plan -> gather -> answer, with the one retry
 */

export { hostFromClient } from "./host";
export type { ExecClient, Host } from "./host";

export { applyOptions, createInquiry } from "./agent";
export type { StartOptions } from "./agent";

export { runInquiry } from "./pipeline";
export type { RunHandlers, RunOptions } from "./pipeline";

export { loadInquiry, saveInquiry } from "./session";
export { buildCorpus, renderCorpus } from "./corpus";
export { cleanPlan, planIsEmpty, PLAN_SCHEMA } from "./plan";
export { executePlan, foundNothing } from "./execute";
export { normalizeOptions, DEFAULT_OPTIONS } from "./options";
export { chat, ChatCancelledError, messageText } from "./chat";

export {
  CHAT,
  LIST_MODELS,
  SEARCH_DOCS,
  DELETE_DOC,
  SESSION_PATH,
  SESSION_TYPE,
  DEFAULT_MAX_DOCS,
  DEFAULT_MAX_PLAN_STEPS,
  DEFAULT_DOC_BUDGET,
  MAX_DOCS_HARD,
  MAX_PLAN_STEPS_HARD,
  MAX_DOC_BUDGET,
  INQUIRY_ROOT,
} from "./refs";

export * from "./types";

/**
 * The pipeline's own types.
 *
 * Field names stay snake_case for anything written into a document
 * (`InquirySession`, its turns and steps), because that document is read back
 * by `solx get doc` and the CLI, and a stored shape that disagrees with the
 * reader's is only a source of confusion. The pure in-memory views the UI
 * consumes -- `StepResult`, `StepView` -- are camelCase, matching the React
 * side of solx-agent.
 */

/**
 * Where the pipeline is. Much shorter than the agent's status set on purpose:
 * inquiry never suspends for approval and never runs an open-ended loop, so
 * the only live states are the three phases. `idle` is the terminal state --
 * the answer is written and the next thing is a person or another question.
 */
export type InquiryStatus = "planning" | "gathering" | "answering" | "idle" | "error";

/** A status is quiescent when nothing is in flight and it is the user's turn. */
export function isQuiescent(status: InquiryStatus): boolean {
  return status !== "planning" && status !== "gathering" && status !== "answering";
}

/** The fixed verb set. There is no tool name here and never will be. */
export type Verb = "search" | "list" | "get" | "types" | "actions";

/** One step of a plan, as the planner produced it and after `plan.ts` cleaned it. */
export interface PlanStep {
  verb: Verb;
  /** Full-text terms for `search`/`types`/`actions`. Two or three words. */
  q?: string | null;
  /** Restrict to a document tree. Copied from the corpus index, not invented. */
  path_prefix?: string | null;
  /** Restrict to one type. Copied from the corpus index, not invented. */
  type_ref?: string | null;
  /** `get` only: the two halves of an exact reference. */
  path?: string | null;
  name?: string | null;
  limit?: number | null;
  /** Set by `plan.ts` when a step was dropped or repaired -- never by the model. */
  note?: string | null;
}

/** What the planner returns, validated by `plan.ts`. */
export interface InquiryPlan {
  steps: PlanStep[];
  /**
   * The model's escape hatch: it already knows the answer, or the question is
   * small talk. When present and there are no steps, no search runs.
   */
  direct?: string | null;
}

/** One document pulled into an answer's grounding set. */
export interface GatheredDoc {
  /** `path/name`, the same spelling a transcript shows and the CLI accepts. */
  ref: string;
  path: string;
  name: string;
  title: string;
  summary: string;
  type_ref: string | null;
  /** `null` when the document store returned a shallow hit rather than a body. */
  contents: unknown;
}

/** How one executed step turned out. */
export type StepOutcome = "ok" | "empty" | "error" | "skipped";

export interface ExecutedStep {
  verb: Verb;
  /** The action actually called, so a transcript shows the mapping. */
  action: string;
  /** The params sent, after coercion and `compact()`. */
  params: Record<string, unknown>;
  outcome: StepOutcome;
  /** How many documents this step contributed (after de-duplication). */
  count: number;
  error: string | null;
  /** A repair or drop note from `plan.ts`, if any. */
  note: string | null;
}

/** One exchange: a question, the plan it produced, and the answer. */
export interface InquiryTurn {
  question: string;
  status: InquiryStatus;
  /** Empty when the planner failed or answered directly. */
  plan: PlanStep[];
  /** Non-null when the plan phase errored; the turn then answers from nothing. */
  plan_error: string | null;
  /** The corpus index shown to the planner, kept so a transcript is inspectable. */
  corpus: string;
  steps: ExecutedStep[];
  documents: GatheredDoc[];
  answer: string | null;
}

/** How an inquiry is configured. Seeded at creation; some is live-editable. */
export interface InquiryOptions {
  /** Maximum documents gathered across all steps of a turn. */
  max_docs: number;
  /** Maximum steps honoured from one plan; extras are dropped with a note. */
  max_plan_steps: number;
  /** Characters of document text handed to the answer phase, across all docs. */
  doc_budget: number;
  /** Whether the corpus index also lists action paths (usually unnecessary). */
  include_actions: boolean;
  /** Whether a plan that returned nothing gets one replan shot. */
  replan: boolean;
  /** Per-model-call HTTP timeout, delegated to solx-ollama. */
  chat_timeout_secs: number | null;
}

/**
 * The stored state of one inquiry. One inquiry is one conversation is one
 * document, under `/inquiry/sessions`, exactly as a session is for the agent.
 */
export interface InquirySession {
  id: string;
  model: string;
  /** Titles the document. Taken from the first question; never a parameter. */
  title: string;
  status: InquiryStatus;
  options: InquiryOptions;
  turns: InquiryTurn[];
  /** The most recent answer, also on the last turn; what a summary carries. */
  answer: string | null;
}

/** What one phase of the pipeline reports back to the caller driving it. */
export interface StepResult {
  session_id: string;
  status: InquiryStatus;
  turn_index: number;
  /** 1-based phase within the turn, for a progress line. */
  phase: number;
  plan: PlanStep[];
  plan_error: string | null;
  steps: ExecutedStep[];
  documents: GatheredDoc[];
  answer: string | null;
  /** Set only when `status` is `error`. */
  error: string | null;
}

export interface OllamaModel {
  name: string;
  capabilities?: string[];
}

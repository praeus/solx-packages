/**
 * Turning an inquiry's turns into what the UI renders.
 *
 * Much simpler than solx-agent's `transcript.ts`, and the reason is
 * structural: the agent stores a flat `messages[]` plus a parallel `calls[]`
 * and has to *reconstruct* the nesting by pairing them positionally. Inquiry
 * stores the nesting already -- each turn holds its plan, its steps, its
 * documents and its answer -- so this is a shape adapter, not a
 * reconstruction. There are no cross-array invariants to rely on and no cursor
 * to keep aligned, which is the whole benefit of a bounded pipeline over an
 * open loop.
 *
 * Free of React so the segmentation is testable without a DOM, the same
 * separation the agent keeps.
 */

import type { ExecutedStep, GatheredDoc, InquiryTurn, PlanStep } from "./harness";

export interface RenderedStep {
  verb: string;
  /** The builtin the verb resolved to, for a tooltip. */
  action: string;
  /** A one-line rendering of the params, for the row. */
  detail: string;
  outcome: ExecutedStep["outcome"];
  count: number;
  error: string | null;
  note: string | null;
}

export interface RenderedTurn {
  key: string;
  question: string;
  plan: PlanStep[];
  planError: string | null;
  steps: RenderedStep[];
  documents: GatheredDoc[];
  answer: string | null;
  /** `status` for a turn still in flight; `null` once it settled. */
  liveStatus: string | null;
}

/** A compact, readable rendering of a step's params for the plan card. */
export function detailOf(step: PlanStep | ExecutedStep): string {
  const parts: string[] = [];
  const q = (step as PlanStep).q;
  if (q) parts.push("q=" + JSON.stringify(q));
  const pp = (step as PlanStep).path_prefix;
  if (pp) parts.push("path=" + pp);
  const tr = (step as PlanStep).type_ref;
  if (tr) parts.push("type=" + tr);
  const path = (step as PlanStep).path;
  const name = (step as PlanStep).name;
  if (path || name) parts.push((path || "") + "/" + (name || ""));
  const limit = (step as PlanStep).limit;
  if (limit) parts.push("limit=" + limit);
  return parts.join("  ");
}

function renderStep(step: ExecutedStep): RenderedStep {
  return {
    verb: step.verb,
    action: step.action,
    detail: detailOf(step),
    outcome: step.outcome,
    count: step.count,
    error: step.error,
    note: step.note,
  };
}

export function buildTurn(turn: InquiryTurn, index: number): RenderedTurn {
  const live = turn.status === "planning" || turn.status === "gathering" || turn.status === "answering";
  return {
    key: "turn-" + index,
    question: turn.question,
    plan: turn.plan || [],
    planError: turn.plan_error,
    steps: (turn.steps || []).map(renderStep),
    documents: turn.documents || [],
    answer: turn.answer,
    liveStatus: live ? turn.status : null,
  };
}

export function buildTurns(turns: InquiryTurn[]): RenderedTurn[] {
  return (turns || []).map(buildTurn);
}

/** One-line recap for a collapsed turn. */
export function summariseTurn(turn: RenderedTurn): string {
  const docs = turn.documents.length;
  const steps = turn.steps.length;
  if (steps === 0) return "answered directly";
  const parts = [
    steps + (steps === 1 ? " lookup" : " lookups"),
    docs + (docs === 1 ? " document" : " documents"),
  ];
  return parts.join(" · ");
}

/** A one-line description of what a turn is doing now, for a live turn. */
export function liveLabel(status: string): string {
  if (status === "planning") return "planning lookups…";
  if (status === "gathering") return "gathering documents…";
  if (status === "answering") return "writing the answer…";
  return "working…";
}

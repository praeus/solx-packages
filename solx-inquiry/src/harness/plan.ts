/**
 * Turning whatever the planner returned into a plan the harness will run.
 *
 * Deliberately the only place a model's output is trusted, and it trusts
 * nothing: a verb outside the fixed set is dropped, a limit is clamped,
 * a missing `get` half is dropped, and every repair is recorded as a `note`
 * rather than failing the turn. Inquiry's whole premise is that a small model
 * cannot be asked to be precise about solx -- so precision is demanded of the
 * schema via `format`, and whatever still slips through is absorbed here
 * instead of being thrown back at the model as an error it must fix.
 *
 * Pure, and free of I/O, so it is testable without a host -- the same
 * separation `solx-agent`'s `transcript.ts` and `harness/` keep.
 */

import {
  DEFAULT_STEP_LIMIT,
  MAX_PLAN_STEPS_HARD,
  MAX_STEP_LIMIT,
  VERBS,
} from "./refs";
import type { InquiryPlan, PlanStep, Verb } from "./types";

/**
 * The JSON Schema handed to `ollama-chat` as `format`, so the planner answers
 * with structure rather than prose. Written by hand rather than derived,
 * because the *descriptions* are the point: they are the only place the model
 * is told what each verb means and that search terms must be short.
 *
 * Note what is absent: no tool names, no action refs, no paths. The model
 * chooses a verb and some words; the harness owns everything else.
 */
export const PLAN_SCHEMA: Record<string, unknown> = {
  type: "object",
  properties: {
    steps: {
      type: "array",
      description:
        "Lookups to run, in order. Use the fewest that answer the question. " +
        "Prefer one search over several.",
      items: {
        type: "object",
        properties: {
          verb: {
            type: "string",
            enum: VERBS,
            description:
              "search: full-text over documents. list: documents under a path or of a type. " +
              "get: one document by exact path and name. types: find a document type. " +
              "actions: find what the system can do.",
          },
          q: {
            type: "string",
            description:
              "Search terms, for search/types/actions. Two or three distinctive words. " +
              "Search terms are ANDed and prefix-matched, so a whole sentence matches nothing.",
          },
          path_prefix: {
            type: "string",
            description:
              "Restrict to a document tree, for search/list. Copy one from the list of known " +
              "paths exactly; do not invent one. Omit to search everywhere.",
          },
          type_ref: {
            type: "string",
            description:
              "Restrict to one document type, for search/list. Copy one from the list of known " +
              "types exactly. Omit unless the question names a kind of thing.",
          },
          path: {
            type: "string",
            description: "For get only: the document's path, for example /blog.",
          },
          name: {
            type: "string",
            description: "For get only: the document's name within that path.",
          },
          limit: {
            type: "integer",
            description: "How many results to return. Default 10.",
          },
        },
        required: ["verb"],
      },
    },
    direct: {
      type: "string",
      description:
        "Only when no lookup is needed, for example a greeting or a question about what you " +
        "can do. Leave out when you have steps.",
    },
  },
};

function asString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

/** Coerce a model's limit into a usable integer, clamped to the hard cap. */
function asLimit(value: unknown): number | null {
  const n = typeof value === "number" ? value : Number(value);
  if (!isFinite(n) || n <= 0) return null;
  return Math.max(1, Math.min(MAX_STEP_LIMIT, Math.floor(n)));
}

/**
 * Clean one raw step. Returns `null` when the step cannot be made to mean
 * anything -- an unknown verb, or a `get` with only one of its halves -- so
 * the caller records a note and moves on.
 */
function cleanStep(raw: unknown): { step: PlanStep | null; note: string | null } {
  if (!raw || typeof raw !== "object") return { step: null, note: "dropped a non-object step" };
  const r = raw as Record<string, unknown>;

  const verb = asString(r.verb) as Verb | null;
  if (!verb || VERBS.indexOf(verb) === -1) {
    return { step: null, note: "dropped a step whose verb was not one of " + VERBS.join(", ") };
  }

  const step: PlanStep = { verb };
  const q = asString(r.q);
  const pathPrefix = asString(r.path_prefix);
  const typeRef = asString(r.type_ref);
  const path = asString(r.path);
  const name = asString(r.name);
  const limit = asLimit(r.limit);

  if (verb === "get") {
    if (!path || !name) {
      return { step: null, note: "dropped a get with no exact path and name" };
    }
    step.path = path;
    step.name = name;
    return { step, note: null };
  }

  if (verb === "search") {
    // A search with no terms is a list; keep it, it is harmless and often
    // what was meant. A search with terms is the normal case.
    if (q) step.q = q;
    if (pathPrefix) step.path_prefix = pathPrefix;
    if (typeRef) step.type_ref = typeRef;
    step.limit = limit ?? DEFAULT_STEP_LIMIT;
    return { step, note: null };
  }

  if (verb === "list") {
    step.q = null;
    if (pathPrefix) step.path_prefix = pathPrefix;
    if (typeRef) step.type_ref = typeRef;
    step.limit = limit ?? DEFAULT_STEP_LIMIT;
    return { step, note: null };
  }

  // types / actions: a keyword search over the type or action registry.
  if (q) step.q = q;
  step.limit = limit ?? DEFAULT_STEP_LIMIT;
  return { step, note: null };
}

export interface CleanedPlan {
  plan: InquiryPlan;
  notes: string[];
  /** Steps that survived, in order. */
  steps: PlanStep[];
}

/**
 * Validate a planner reply into a runnable plan.
 *
 * `cap` bounds how many steps are honoured; extras are dropped with a note so
 * a transcript shows the model asked for more than was run, rather than
 * silently doing less than it planned.
 */
export function cleanPlan(raw: unknown, cap: number, hardCap = MAX_PLAN_STEPS_HARD): CleanedPlan {
  const notes: string[] = [];
  if (!raw || typeof raw !== "object") {
    return { plan: { steps: [], direct: null }, notes: ["planner returned nothing usable"], steps: [] };
  }
  const r = raw as Record<string, unknown>;
  const rawSteps = Array.isArray(r.steps) ? r.steps : [];
  const limit = Math.max(1, Math.min(hardCap, Math.floor(cap) || 1));

  const steps: PlanStep[] = [];
  for (const candidate of rawSteps) {
    if (steps.length >= limit) {
      notes.push("dropped " + (rawSteps.length - steps.length) + " step(s) beyond the cap of " + limit);
      break;
    }
    const { step, note } = cleanStep(candidate);
    if (note) notes.push(note);
    if (step) steps.push(step);
  }

  const direct = asString(r.direct);
  // A `direct` alongside real steps is a contradiction; the steps win,
  // because running them costs little and answering without them would be
  // guessing. Only a stepless `direct` short-circuits the pipeline.
  const keptDirect = steps.length === 0 ? direct ?? null : null;
  return { plan: { steps, direct: keptDirect }, notes, steps };
}

/** True when a plan does nothing and says nothing -- the replan trigger. */
export function planIsEmpty(plan: InquiryPlan): boolean {
  return plan.steps.length === 0 && !plan.direct;
}

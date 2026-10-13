/**
 * The pipeline: plan, gather, answer -- and, once, replan.
 *
 * This is inquiry's whole loop, and it is deliberately not a loop in the
 * agent's sense. There is no iteration counter the model drives, no catalogue
 * re-resolved per turn, no approval that suspends the run. There are three
 * phases with a fixed order, and at most one retry of the first two. That
 * boundedness is the point: a small model asked to plan is being asked one
 * clear question, and the harness -- not the model -- decides when it is done.
 *
 * Each phase persists the document, so a browser tab closed mid-gather leaves
 * a true record, exactly as the agent's loop does. The difference is how often
 * it matters: three writes per turn, not several per tool call.
 *
 * ## The one retry
 *
 * If a plan runs and finds nothing, the pipeline replans once with the failure
 * fed back, then executes that. One retry, never two: a model that found
 * nothing twice will not find something on the third try, and the honest
 * answer ("I could not find that") is more useful than a fourth attempt. The
 * retry is skipped when the operator turned it off, or when the first plan was
 * empty for a reason -- an empty plan means the planner chose `direct`, and
 * re-asking would only repeat it.
 */

import { chat, ChatCancelledError, messageText, type ChatRequest } from "./chat";
import { buildCorpus } from "./corpus";
import { executePlan } from "./execute";
import { cleanPlan, PLAN_SCHEMA, planIsEmpty } from "./plan";
import {
  ANSWER_PREAMBLE,
  answerMessage,
  PLANNER_BRIEF,
  plannerMessage,
  renderDoc,
  renderHistory,
} from "./prompt";
import { saveInquiry } from "./session";
import { DEFAULT_DOC_BUDGET, DOC_EXCERPT_CAP } from "./refs";
import { normalizeOptions } from "./options";
import type { Host } from "./host";
import type {
  ExecutedStep,
  GatheredDoc,
  InquirySession,
  InquiryTurn,
  StepResult,
} from "./types";

export interface RunHandlers {
  /** After every phase, with the session as it now stands. */
  onProgress(result: StepResult, session: InquirySession): void;
}

export interface RunOptions {
  /** Checked between phases; stops the pipeline without cancelling the answer. */
  isAbandoned?: () => boolean;
}

function summarize(session: InquirySession, turn: InquiryTurn, error: string | null): StepResult {
  return {
    session_id: session.id,
    status: session.status,
    turn_index: session.turns.length - 1,
    phase: session.status === "planning" ? 1 : session.status === "gathering" ? 2 : session.status === "answering" ? 3 : 3,
    plan: turn.plan,
    plan_error: turn.plan_error,
    steps: turn.steps,
    documents: turn.documents,
    answer: turn.answer,
    error,
  };
}

/** The request for a plan call: structured output, no tools, no free text. */
function planRequest(session: InquirySession, messages: { role: string; content: string }[]): ChatRequest {
  return {
    model: session.model,
    messages: messages as ChatRequest["messages"],
    format: PLAN_SCHEMA,
    timeout_secs: session.options.chat_timeout_secs,
  };
}

/** The request for the answer call: free text, no tools, no format. */
function answerRequest(session: InquirySession, messages: { role: string; content: string }[]): ChatRequest {
  return {
    model: session.model,
    messages: messages as ChatRequest["messages"],
    timeout_secs: session.options.chat_timeout_secs,
  };
}

/** Ask the planner, once. Returns the raw reply text or throws. */
async function askPlanner(
  host: Host,
  session: InquirySession,
  messages: { role: string; content: string }[],
  isAbandoned?: () => boolean,
): Promise<unknown> {
  const reply = await chat(host, planRequest(session, messages), isAbandoned);
  const text = messageText(reply);
  if (!text.trim()) return { steps: [] };
  try {
    return JSON.parse(text);
  } catch {
    // A model that ignored `format` and wrote prose lands here. Treat it as a
    // plan with no steps rather than erroring -- the turn then answers from
    // nothing, which is the honest outcome, and the raw text is not worth
    // parsing heuristically.
    return { steps: [], direct: text };
  }
}

/** Render the gathered documents within the turn's character budget. */
function renderDocuments(documents: GatheredDoc[], budget: number): string {
  const blocks: string[] = [];
  let used = 0;
  for (const doc of documents) {
    if (used >= budget) break;
    // Each document gets an equal share of what is left, so one huge document
    // cannot starve the rest -- and DOC_EXCERPT_CAP keeps a single slice sane.
    const remaining = budget - used;
    const share = Math.min(DOC_EXCERPT_CAP, Math.max(400, Math.floor(remaining / 2)));
    const block = renderDoc(doc, share);
    blocks.push(block);
    used += block.length;
  }
  return blocks.join("\n\n");
}

/**
 * Run one question through the pipeline. Mutates and persists `session`.
 *
 * Returns the final `StepResult`. A `ChatCancelledError` from any phase is
 * turned into a clean `error` turn rather than propagating, so a Stop leaves
 * the document consistent rather than half-written.
 */
export async function runInquiry(
  host: Host,
  session: InquirySession,
  question: string,
  handlers: RunHandlers,
  opts: RunOptions = {},
): Promise<StepResult> {
  session.options = normalizeOptions(session.options);
  const budget = session.options.doc_budget || DEFAULT_DOC_BUDGET;

  const corpus = await buildCorpus(host, session.options.include_actions);
  const history = renderHistory(session.turns);

  const turn: InquiryTurn = {
    question,
    status: "planning",
    plan: [],
    plan_error: null,
    corpus: corpus.text,
    steps: [],
    documents: [],
    answer: null,
  };
  session.turns.push(turn);
  session.status = "planning";
  session.answer = null;
  await saveInquiry(host, session.id, session);
  handlers.onProgress(summarize(session, turn, null), session);

  try {
    // ── phase 1: plan ──────────────────────────────────────────────────────
    const planMessages = [
      { role: "system", content: PLANNER_BRIEF },
      { role: "user", content: plannerMessage(question, corpus.text, history) },
    ];
    const cleaned = cleanPlan(
      await askPlanner(host, session, planMessages, opts.isAbandoned),
      session.options.max_plan_steps,
    );
    turn.plan = cleaned.steps;

    // ── phase 2: gather ────────────────────────────────────────────────────
    session.status = "gathering";
    turn.status = "gathering";
    await saveInquiry(host, session.id, session);
    handlers.onProgress(summarize(session, turn, null), session);

    let executed = await executePlan(host, turn.plan, session.options.max_docs);
    let steps: ExecutedStep[] = executed.steps;

    // The one retry: a plan that did something and found nothing gets a second
    // shot with the failure named. An empty plan is not retried -- it means the
    // planner chose `direct`, and re-asking would only repeat it.
    const canReplan = session.options.replan && !planIsEmpty({ steps: turn.plan, direct: null });
    if (canReplan && executed.documents.length === 0 && !opts.isAbandoned?.()) {
      const retryMessages = [
        ...planMessages,
        {
          role: "user",
          content:
            "Those lookups found no documents. Try again with different terms, a broader " +
            "path, or a different verb. Do not repeat a step that just failed.",
        },
      ];
      const retryPlan = cleanPlan(
        await askPlanner(host, session, retryMessages, opts.isAbandoned),
        session.options.max_plan_steps,
      );
      if (retryPlan.steps.length > 0) {
        const retried = await executePlan(host, retryPlan.steps, session.options.max_docs);
        // The retry's steps replace the plan in the transcript, because they
        // are what actually produced the documents; the first attempt is not
        // worth rendering alongside a successful second one.
        turn.plan = retryPlan.steps;
        steps = retried.steps;
        executed = retried;
      }
    }

    turn.steps = steps;
    turn.documents = executed.documents;

    // ── phase 3: answer ────────────────────────────────────────────────────
    session.status = "answering";
    turn.status = "answering";
    await saveInquiry(host, session.id, session);
    handlers.onProgress(summarize(session, turn, null), session);

    const documents = renderDocuments(executed.documents, budget);
    const answerMessages = [
      { role: "system", content: ANSWER_PREAMBLE },
      { role: "user", content: answerMessage(question, history, documents) },
    ];

    // A `direct` plan answer short-circuits the writer: the planner already
    // decided no lookup was needed and said what to say. Re-asking the same
    // model to rewrite its own sentence would only add latency.
    const direct = cleaned.plan.direct;
    let answer: string;
    if (direct && turn.plan.length === 0) {
      answer = direct;
    } else {
      const reply = await chat(host, answerRequest(session, answerMessages), opts.isAbandoned);
      answer = messageText(reply).trim();

      // An empty reply from a model that had documents is a real failure worth
      // surfacing, but it must not throw away the plan and documents that ran.
      if (!answer && executed.documents.length > 0) {
        answer =
          "The model returned no answer. The documents it was given are listed above; " +
          "try asking again, or rephrase the question.";
      } else if (!answer) {
        answer = "I could not find anything about that. Try naming a path or a different term.";
      }
    }

    turn.answer = answer;
    turn.status = "idle";
    session.status = "idle";
    session.answer = answer;
    await saveInquiry(host, session.id, session);
    const result = summarize(session, turn, null);
    handlers.onProgress(result, session);
    return result;
  } catch (e) {
    const cancelled = e instanceof ChatCancelledError;
    const message = cancelled
      ? "stopped"
      : String((e as Error)?.message ?? e);
    turn.status = "error";
    turn.answer = turn.answer ?? null;
    session.status = "error";
    await saveInquiry(host, session.id, session);
    const result = summarize(session, turn, message);
    handlers.onProgress(result, session);
    return result;
  }
}

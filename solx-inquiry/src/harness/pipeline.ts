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
 * If a turn has nothing to answer from, the pipeline replans once with the
 * failure fed back, then executes that. Two failures count: a plan that ran and
 * found no usable document, and an empty plan -- the planner failing to answer
 * at all. One retry, never two: a model that produced nothing twice will not
 * produce something on the third try, and the honest answer ("I could not find
 * that") is more useful than a fourth attempt. The retry is skipped when the
 * operator turned it off, and when the plan carried a `direct` answer, because
 * that is a deliberate decision not to look anything up rather than a failure.
 */

import { chat, ChatCancelledError, messageText, type ChatRequest } from "./chat";
import { buildCorpus, vocabularyOf } from "./corpus";
import { executePlan } from "./execute";
import { cleanPlan, parsePlanReply, PLAN_SCHEMA } from "./plan";
import {
  ANSWER_PREAMBLE,
  answerMessage,
  PLANNER_BRIEF,
  plannerMessage,
  renderDoc,
  renderHistory,
} from "./prompt";
import { saveInquiry } from "./session";
import { DEFAULT_DOC_BUDGET, DOC_EXCERPT_CAP, refOf, SESSION_PATH } from "./refs";
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

/**
 * Settle a turn whatever happened, and persist it.
 *
 * The invariant: **a turn never ends live.** `planning`, `gathering` and
 * `answering` are states a session is only ever in *while* this function is
 * running, and this is the one place that guarantees the session leaves them.
 *
 * Why it is a helper rather than the two `saveInquiry` calls it replaces: the
 * failure mode is a session stuck in a live state with no way out. If the
 * process dies mid-phase -- a killed tab, a cancelled MCP call, a Ctrl-C, a
 * power cut -- nothing else on the read path will finish it, and `loadInquiry`
 * has no resume: a later `solx exec ... --json '{"session_id":...}'` would
 * report a state that is not true and then leave it that way. The agent has a
 * documented `pending[]` resume for exactly this; inquiry has no half-finished
 * state worth resuming, so the honest resolution is to close the turn out.
 *
 * Called on the success path *and* from the catch-all, which is what makes the
 * catch-all worth having at all -- without this, an abandoned `await` between
 * phases escapes to nobody and the document lies.
 */
async function settle(
  host: Host,
  session: InquirySession,
  turn: InquiryTurn,
  status: "idle" | "error",
  answer: string | null,
  message: string | null,
  handlers: RunHandlers,
): Promise<StepResult> {
  turn.status = status;
  turn.answer = answer;
  if (message !== null) turn.plan_error = turn.plan_error ?? message;
  // Session-level state belongs to the newest turn. A Stopped run settles
  // late -- after its in-flight call returns -- and by then the widget may have
  // started the next question on the same session object; overwriting
  // `status` here would mark that live turn finished.
  if (session.turns[session.turns.length - 1] === turn) {
    session.status = status;
    session.answer = answer;
  }
  await saveInquiry(host, session.id, session);
  const result = summarize(session, turn, message);
  handlers.onProgress(result, session);
  return result;
}

/**
 * Throw at a phase boundary when the caller has abandoned the run.
 *
 * `chat` checks between polls, but the gather phase and every save are plain
 * awaits that never look -- so without these a Stop during planning would
 * still run every lookup and write `gathering`/`answering` before the answer
 * call finally noticed. Thrown as a cancellation so the catch settles it as
 * "stopped".
 */
function checkAbandoned(opts: RunOptions): void {
  if (opts.isAbandoned?.()) throw new ChatCancelledError();
}


/** The request for the answer call: free text, no tools, no format. */
function answerRequest(session: InquirySession, messages: { role: string; content: string }[]): ChatRequest {
  return {
    model: session.model,
    messages: messages as ChatRequest["messages"],
    timeout_secs: session.options.chat_timeout_secs,
  };
}

/**
 * Ask the planner, once: structured output, no tools, no thinking. Returns the
 * parsed reply for `cleanPlan`, or throws.
 *
 * `think: false` is the single largest latency lever in the package. A thinking
 * model (qwen3 and its kin, the common local choice) otherwise reasons at
 * length *before* emitting a plan the schema already constrains to a verb and
 * a few words. Measured against a real install: the same planner call took
 * 125s by default on a warm model (20,100 characters of thinking) and 2s with
 * thinking off, with an equivalent plan. The writer
 * keeps the model's default -- summarizing several documents is where any
 * reasoning is worth paying for.
 *
 * Exported so the widget's plan preview and the headless `plan_only` mode make
 * the identical call rather than three copies drifting apart.
 */
export async function requestPlan(
  host: Host,
  model: string,
  timeoutSecs: number | null,
  messages: { role: string; content: string }[],
  isAbandoned?: () => boolean,
): Promise<unknown> {
  const request: ChatRequest = {
    model,
    messages: messages as ChatRequest["messages"],
    format: PLAN_SCHEMA,
    think: false,
    timeout_secs: timeoutSecs,
  };
  let reply;
  try {
    reply = await chat(host, request, isAbandoned);
  } catch (e) {
    // Defensive: Ollama checks the capability only for `think: true`, but a
    // server that refuses the field on a non-thinking model must not cost the
    // turn -- ask again without it.
    if (!/support(s)? thinking/i.test(String((e as Error)?.message ?? e))) throw e;
    reply = await chat(host, { ...request, think: undefined }, isAbandoned);
  }
  return parsePlanReply(messageText(reply));
}

function askPlanner(
  host: Host,
  session: InquirySession,
  messages: { role: string; content: string }[],
  isAbandoned?: () => boolean,
): Promise<unknown> {
  return requestPlan(host, session.model, session.options.chat_timeout_secs, messages, isAbandoned);
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
  const vocab = vocabularyOf(corpus);
  const self = [refOf(SESSION_PATH, session.id)];
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
    checkAbandoned(opts);

    // ── phase 2: gather ────────────────────────────────────────────────────
    session.status = "gathering";
    turn.status = "gathering";
    await saveInquiry(host, session.id, session);
    handlers.onProgress(summarize(session, turn, null), session);

    let executed = await executePlan(host, turn.plan, session.options.max_docs, vocab, self);
    let steps: ExecutedStep[] = executed.steps;

    // One retry when the turn has nothing to answer from. Two different
    // failures land here and both deserve it:
    //
    //   - the plan ran and found no usable document (a phrase that ANDs to
    //     nothing, or a path that holds nothing);
    //   - the plan produced no steps at all, which is the planner failing to
    //     answer rather than deciding no lookup was needed. Found by running
    //     the package: a model returned `{}` for a clear question, and the turn
    //     answered "I could not find that" without ever having searched.
    //
    // A `direct` answer is the one case that must NOT be retried -- the planner
    // deliberately chose not to look anything up, and re-asking would only
    // repeat it.
    const answeredDirectly = !!cleaned.plan.direct && turn.plan.length === 0;
    const canReplan =
      session.options.replan && !answeredDirectly && executed.documents.length === 0 && !opts.isAbandoned?.();

    if (canReplan) {
      const retryNote =
        turn.plan.length === 0
          ? "You returned no lookups. Re-read the question and propose one or two lookups that could answer it."
          : "Those lookups found no documents. Try again with different terms, a broader path, " +
            "or a different verb. Do not repeat a step that just failed.";
      const retryMessages = [...planMessages, { role: "user", content: retryNote }];
      const retryPlan = cleanPlan(
        await askPlanner(host, session, retryMessages, opts.isAbandoned),
        session.options.max_plan_steps,
      );
      checkAbandoned(opts);
      if (retryPlan.steps.length > 0) {
        const retried = await executePlan(host, retryPlan.steps, session.options.max_docs, vocab, self);
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
    checkAbandoned(opts);

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

    // A Stop that landed while the answer call was finishing: the reply came
    // back, but the caller has moved on, so the turn is recorded as stopped.
    checkAbandoned(opts);

    turn.answer = answer;
    return settle(host, session, turn, "idle", answer, null, handlers);
  } catch (e) {
    const cancelled = e instanceof ChatCancelledError;
    const message = cancelled ? "stopped" : String((e as Error)?.message ?? e);
    return settle(host, session, turn, "error", turn.answer, message, handlers);
  }
}

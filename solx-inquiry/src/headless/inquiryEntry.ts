/**
 * `inquiry`: the headless entry point for the pipeline, run as a Command
 * action's Node child. Params arrive as JSON on stdin (the Command action
 * contract); the result is written to stdout as JSON.
 *
 * A thin wrapper, not a second implementation: every call goes through the
 * same `src/harness/*` code `InquiryWidget.tsx` drives, via an HTTP `Host`
 * that mirrors `solx-agent`'s headless entry almost verbatim. The differences
 * are all subtractions -- there is no `approve` parameter, because inquiry
 * never suspends for approval, and no `grant`, because every verb is a read.
 *
 * One question is one turn. Pass the result's `session_id` back with a new
 * question to continue the conversation, so a caller can drive a whole thread
 * across repeated `solx exec` calls the same way the widget drives it across
 * page reloads.
 */

import {
  applyOptions,
  buildCorpus,
  vocabularyOf,
  cleanPlan,
  createInquiry,
  executePlan,
  hostFromClient,
  loadInquiry,
  normalizeOptions,
  requestPlan,
  runInquiry,
} from "../harness/index";
import { PLANNER_BRIEF, plannerMessage } from "../harness/prompt";
import type { Host, InquiryOptions } from "../harness/index";
import { resolveServerConfig } from "./serverConfig";

interface InquiryParams {
  question?: string;
  model?: string;
  session_id?: string;
  path_prefix?: string;
  type_ref?: string;
  max_docs?: number;
  max_plan_steps?: number;
  doc_budget?: number;
  include_actions?: boolean;
  replan?: boolean;
  chat_timeout_secs?: number;
  /** Run the plan and gather, but return without the answer call. For tuning. */
  plan_only?: boolean;
}

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
  return Buffer.concat(chunks).toString("utf8");
}

function buildHost(): Host {
  const cfg = resolveServerConfig();
  return hostFromClient({
    actions: {
      async exec(path: string, name: string, params?: unknown) {
        const ref = (path === "/" ? "" : path) + "/" + name;
        const res = await fetch(cfg.serverUrl + "/actions" + ref, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: "Bearer " + cfg.serverToken },
          body: JSON.stringify(params ?? {}),
        });
        if (!res.ok) return { success: false, message: await res.text(), result: null };
        return (await res.json()) as { success?: boolean; message?: string | null; result?: unknown };
      },
    },
  });
}

const noopProgress = { onProgress: () => {} };

/** Turn the loose CLI knobs into option overrides plus a question scope. */
function optionsFrom(params: InquiryParams): Partial<InquiryOptions> {
  const o: Partial<InquiryOptions> = {};
  if (params.max_docs !== undefined) o.max_docs = params.max_docs;
  if (params.max_plan_steps !== undefined) o.max_plan_steps = params.max_plan_steps;
  if (params.doc_budget !== undefined) o.doc_budget = params.doc_budget;
  if (params.include_actions !== undefined) o.include_actions = params.include_actions;
  if (params.replan !== undefined) o.replan = params.replan;
  if (params.chat_timeout_secs !== undefined) o.chat_timeout_secs = params.chat_timeout_secs;
  return o;
}

/**
 * A path/type hint is folded into the question text rather than into an option,
 * because the pipeline deliberately has no "scope" field -- the planner decides
 * where to look, and a caller-supplied hint is just another thing for it to
 * consider. This is the one place the headless port adds vocabulary the widget
 * does not, and it is only because a script often knows the path already.
 */
function withHints(question: string, params: InquiryParams): string {
  const hints: string[] = [];
  if (params.path_prefix) hints.push("focus on documents under " + params.path_prefix);
  if (params.type_ref) hints.push("focus on documents of type " + params.type_ref);
  if (hints.length === 0) return question;
  return question + "\n\n(" + hints.join("; ") + ".)";
}

/**
 * Plan and gather without answering, and without writing anything: report what
 * the model asked for and what came back. Implemented here rather than in the
 * pipeline so the widget never sees this mode -- it exists to let a person tune
 * prompts from the CLI, not to be a product feature. Persisting a session for
 * it would only leave an empty inquiry behind on every tuning run.
 */
async function planOnly(host: Host, params: InquiryParams, overrides: Partial<InquiryOptions>) {
  if (!params.question) throw new Error("question is required for plan_only");
  const base = params.session_id ? (await loadInquiry(host, params.session_id)).options : null;
  const options = normalizeOptions({ ...(base || {}), ...overrides });
  const question = withHints(params.question, params);

  const corpus = await buildCorpus(host, options.include_actions);
  const raw = await requestPlan(host, params.model!, options.chat_timeout_secs, [
    { role: "system", content: PLANNER_BRIEF },
    { role: "user", content: plannerMessage(question, corpus.text, "") },
  ]);
  const cleaned = cleanPlan(raw, options.max_plan_steps);
  const executed = await executePlan(host, cleaned.steps, options.max_docs, vocabularyOf(corpus));
  return {
    session_id: params.session_id ?? null,
    plan: cleaned.steps,
    direct: cleaned.plan.direct ?? null,
    notes: cleaned.notes,
    steps: executed.steps,
    documents: executed.documents.map((d) => d.ref),
  };
}

async function run(params: InquiryParams) {
  if (!params.model) throw new Error("model is required");
  const host = buildHost();
  const options = optionsFrom(params);

  if (params.plan_only) return planOnly(host, params, options);

  let session;
  if (params.session_id) {
    session = await loadInquiry(host, params.session_id);
    session.model = params.model;
    applyOptions(session, options);
    if (!params.question) {
      // No question on a continued session: nothing to do but report state.
      return {
        session_id: session.id,
        status: session.status,
        answer: session.answer,
        turns: session.turns.length,
      };
    }
  } else {
    if (!params.question) throw new Error("question is required to start a new inquiry");
    session = await createInquiry(host, params.question, { model: params.model, options });
  }

  const question = withHints(params.question || "", params);
  return runInquiry(host, session, question, noopProgress);
}

async function main() {
  const raw = await readStdin();
  const params = raw.trim() ? (JSON.parse(raw) as InquiryParams) : ({} as InquiryParams);
  const result = await run(params);
  process.stdout.write(JSON.stringify(result));
}

main().catch((err) => {
  process.stderr.write(String((err && (err as Error).stack) || err));
  process.exitCode = 1;
});

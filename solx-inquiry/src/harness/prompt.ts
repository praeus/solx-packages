/**
 * The two prompts, and the conversation history that feeds them.
 *
 * Like solx-agent, this harness injects no base prompt of its own beyond what
 * is assembled here -- but unlike the agent, the framing is *not* the whole
 * behavioural contract, because the model is not choosing tools. It is
 * choosing among five verbs and writing a final answer, both of which the
 * schema and the documents constrain far more tightly than prose can. So the
 * briefs here are short and structural: what the verbs mean, and that search
 * terms are ANDed and must be few.
 *
 * The one thing the brief must teach is the thing solx punishes: a whole
 * sentence as a search query matches nothing, and an invented path finds
 * nothing. Both are stated once, plainly, and the corpus index supplies the
 * real paths so the model does not have to invent them.
 */

import { HISTORY_TEXT_CAP, HISTORY_TURNS } from "./refs";
import { truncate } from "./text";
import type { InquiryTurn } from "./types";

/** How the planner is told what it is doing. Short by design -- see the file note. */
export const PLANNER_BRIEF = `You are planning how to answer a question about the documents in a solx instance.

You cannot read documents directly. You choose a few lookups; another step runs them and
gives the results to a writer. Your only job is to choose the lookups.

You have exactly these verbs:

  search   full-text search over documents. Use it whenever the question is about content.
           Give two or three distinctive words in "q" -- never a sentence. Search terms
           are ANDed and prefix-matched, so "how do I set up the server" matches nothing.
  list     the documents under a path or of a type, no search terms. Use it to enumerate.
  get      one document by its exact path and name, when the question names one.
  types    find a document type by name.
  actions  find what this solx instance can do, by name.

Rules that matter:

  Prefer one good search over several. Two or three steps is usually enough.
  Use the paths and types listed below exactly as written. Do not invent a path; if none
  looks relevant, search everywhere by leaving path_prefix out.
  Ask for the fewest documents that could answer the question. The results are read by a
  writer with limited room, so ten focused documents beat forty loose ones.
  Earlier questions asked here are stored under /inquiry/sessions. Search there only when
  the question is about past questions; it never holds the answer to anything else.
  If the question needs no lookup at all -- a greeting, or a question about what you can
  do -- reply with only a "direct" answer and no steps.`;

/** How the answer phase is told what to do. */
export const ANSWER_PREAMBLE = `You answer a question using the documents provided below, and the earlier turns of this
conversation when there are any.

The documents are your source for facts. Answer from them; do not use outside knowledge to
fill gaps, and do not invent a document that is not there. If the question follows on from
an earlier turn, the earlier question and answer are shown to you -- you may rely on what
you already established there, and you do not need a new document to do so.

If neither the documents nor the conversation contain the answer, say so plainly and briefly
-- name what you looked at, and, if a path or type seemed close but not right, say that. A
short "I could not find that" is a correct answer and a useful one.

When documents disagree, say so. When a document is only partly shown, note that it
continues rather than treating the excerpt as the whole.

Match the shape of the answer to the question: a question wanting a list gets a list, one
wanting a paragraph gets a paragraph. Cite the document by its reference (the path and name
shown above it) when you state a fact from it.`;

/**
 * The last few turns, rendered for the planner or the answer phase.
 *
 * Bounded on both axes -- how many turns and how large each -- because this is
 * re-sent on every call and grows without limit otherwise. Only the question
 * and the answer are kept: the plan and the documents of earlier turns are
 * not useful context for this one's lookup, and re-sending them would spend
 * the budget the current documents need.
 */
export function renderHistory(turns: InquiryTurn[]): string {
  const recent = turns.filter((t) => t.answer).slice(-HISTORY_TURNS);
  if (recent.length === 0) return "";
  const lines: string[] = ["Earlier in this conversation:"];
  for (const t of recent) {
    lines.push("Q: " + truncate(t.question, HISTORY_TEXT_CAP));
    lines.push("A: " + truncate(t.answer, HISTORY_TEXT_CAP));
  }
  return lines.join("\n");
}

/**
 * The plan-phase user message: the question, the corpus index, and any
 * history, in that order so the question is what the model reads first.
 */
export function plannerMessage(question: string, corpus: string, history: string): string {
  const parts: string[] = [];
  parts.push("Question: " + question);
  if (history) parts.push("", history);
  parts.push("", corpus);
  return parts.join("\n");
}

/**
 * One document, rendered for the answer prompt.
 *
 * The reference goes first because the answer preamble asks the writer to cite
 * by it, and the summary is included ahead of the body because for a document
 * whose body is a large JSON blob the summary is often the more useful line.
 */
export function renderDoc(
  doc: { ref: string; title: string; summary: string; contents: unknown },
  bodyCap: number,
): string {
  const head = doc.title ? "=== " + doc.ref + " — " + doc.title + " ===" : "=== " + doc.ref + " ===";
  const lines = [head];
  if (doc.summary) lines.push(doc.summary);
  if (doc.contents !== null && doc.contents !== undefined) {
    const body =
      typeof doc.contents === "string" ? doc.contents : safeStringify(doc.contents);
    if (body) lines.push(body);
  }
  return truncate(lines.join("\n"), bodyCap);
}

/** The answer-phase user message: the question, then the documents. */
export function answerMessage(question: string, history: string, documents: string): string {
  const parts: string[] = [];
  if (history) parts.push(history, "");
  parts.push("Question: " + question);
  parts.push("");
  if (documents) {
    parts.push("Documents:", "", documents);
  } else {
    parts.push("Documents: (none found for this question)");
  }
  return parts.join("\n");
}

function safeStringify(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

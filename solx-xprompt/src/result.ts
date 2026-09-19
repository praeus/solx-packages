/**
 * Making a `multi_inquire` result safe to render.
 *
 * `TurnBlock` reads `result.intent.mode`, `result.hits`, `result.responses`
 * and `result.scripts` directly, and a React render that throws unmounts the
 * whole tree. Because the transcript is persisted to localStorage, a single
 * bad turn then survives a reload — the widget comes back blank, with no
 * "Clear" button left to press. So every result is put through here before it
 * can reach a component, from all three directions it arrives from: a live
 * turn (`dispatch.waitForResult`), the localStorage transcript, and a session
 * document replayed by `session.loadSessionTranscript`.
 *
 * This repairs rather than rejects wherever it can. Results are structural
 * only — solx-server never validates result shapes (see `types.ts`) — and a
 * turn stored by an older build is missing whole fields rather than being
 * nonsense. A missing `scripts[]` should cost the scripts, not the answer.
 * `null` comes back only when there is nothing recognisable to render.
 */

import type {
  InquireHit,
  MultiInquireIntent,
  MultiInquireResponse,
  MultiInquireResult,
  MultiInquireScript,
  ScriptStep,
  Turn,
} from "./types";

/**
 * A result every component in this widget can render without guards, or
 * `null` when the value is not a result at all.
 */
export function normalizeResult(value: unknown): MultiInquireResult | null {
  const raw = record(value);
  if (!raw) return null;
  return {
    instruction: str(raw.instruction) ?? "",
    model: str(raw.model) ?? "",
    session: str(raw.session) ?? "",
    intent: normalizeIntent(raw.intent),
    responses: rows(raw.responses).map(normalizeResponse),
    memories: Array.isArray(raw.memories) ? raw.memories : [],
    scripts: rows(raw.scripts).map(normalizeScript),
    next_prompt: str(raw.next_prompt) ?? null,
    hits: rows(raw.hits).map(normalizeHit),
    notes: strings(raw.notes),
    errors: Array.isArray(raw.errors) ? raw.errors : [],
    session_document: (raw.session_document as MultiInquireResult["session_document"]) ?? null,
  };
}

/**
 * One transcript entry, or `null` to drop it.
 *
 * Dropping is right for a transcript entry in a way it would not be for a
 * live result: the alternative is rendering a card that says nothing, and the
 * entry is already unreadable by the time we get here.
 */
export function normalizeTurn(value: unknown): Turn | null {
  const raw = record(value);
  if (!raw) return null;
  const at = str(raw.at) ?? "";
  switch (raw.kind) {
    case "user": {
      const text = str(raw.text);
      return text === undefined ? null : { kind: "user", text, at };
    }
    case "answer": {
      const result = normalizeResult(raw.result);
      return result === null ? null : { kind: "answer", model: str(raw.model) ?? "", result, at };
    }
    case "run": {
      const status = raw.status;
      return {
        kind: "run",
        status: status === "error" || status === "skipped" ? status : "ok",
        message: str(raw.message) ?? "",
        at,
      };
    }
    case "error":
      return { kind: "error", message: str(raw.message) ?? "(no message)", at };
    default:
      return null;
  }
}

/** Every renderable entry of a stored transcript, malformed ones dropped. */
export function normalizeTranscript(value: unknown): Turn[] {
  if (!Array.isArray(value)) return [];
  return value.map(normalizeTurn).filter((t): t is Turn => t !== null);
}

function normalizeIntent(value: unknown): MultiInquireIntent {
  const raw = record(value);
  // `direct` is the safe default for a missing mode: it renders the responses
  // and opens no fan-out affordances, whereas defaulting to `inquire` would
  // promise research the turn may never have done.
  const mode = raw?.mode === "inquire" ? "inquire" : "direct";
  return {
    mode,
    response: str(raw?.response) ?? null,
    memory: raw?.memory === true,
    inquiries: rows(raw?.inquiries).map((one) => ({
      kind: str(one.kind) ?? "documents",
      question: str(one.question) ?? "",
      terms: strings(one.terms),
      prompt: str(one.prompt) ?? null,
    })),
    next_prompt: str(raw?.next_prompt) ?? null,
  };
}

function normalizeResponse(raw: Record<string, unknown>): MultiInquireResponse {
  return {
    text: str(raw.text) ?? "",
    title: str(raw.title) ?? null,
    memory: raw.memory === true,
    tags: strings(raw.tags),
    citations: strings(raw.citations),
    inquiry: typeof raw.inquiry === "number" ? raw.inquiry : null,
  };
}

function normalizeScript(raw: Record<string, unknown>): MultiInquireScript {
  return {
    title: str(raw.title) ?? null,
    actions: strings(raw.actions),
    // Load-bearing, not cosmetic: `runScript` gates destructive steps on this
    // list, so a missing one must become empty rather than undefined.
    destructive: strings(raw.destructive),
    notes: strings(raw.notes),
    steps: rows(raw.steps).map(normalizeStep),
  };
}

function normalizeStep(raw: Record<string, unknown>): ScriptStep {
  return {
    action_ref: str(raw.action_ref) ?? "",
    params: record(raw.params) ?? {},
    capture: str(raw.capture) ?? null,
  };
}

function normalizeHit(raw: Record<string, unknown>): InquireHit {
  const details = record(raw.details);
  return {
    source: raw.source === "action" ? "action" : "document",
    path: str(raw.path) ?? "",
    name: str(raw.name) ?? "",
    title: str(raw.title) ?? null,
    summary: str(raw.summary) ?? null,
    score: typeof raw.score === "number" ? raw.score : undefined,
    matched_terms: strings(raw.matched_terms),
    inquiry: typeof raw.inquiry === "number" ? raw.inquiry : null,
    details: details
      ? {
          category: str(details.category),
          capabilities: strings(details.capabilities),
          actionType: str(details.actionType),
          phrases: strings(details.phrases),
          paramTypeRef: str(details.paramTypeRef),
          paramSchema: record(details.paramSchema),
          contents: details.contents,
        }
      : null,
  };
}

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

/** Array elements that are objects; anything else in the array is dropped. */
function rows(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return [];
  return value.filter((v): v is Record<string, unknown> => record(v) !== undefined);
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

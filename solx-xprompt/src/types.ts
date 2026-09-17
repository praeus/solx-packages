/**
 * Shapes that cross the widget boundary. These mirror the documented
 * multi_inquire / ollama result schemas (solx-server never validates result
 * shapes, so these are structural only — fields are read defensively).
 */

export interface OllamaModel {
  name: string;
  size?: number;
  capabilities?: string[];
}

export interface InquireHit {
  source: "document" | "action";
  path: string;
  name: string;
  title?: string | null;
  summary?: string | null;
  score?: number;
  matched_terms?: string[];
  /** Which inquiry (by index) produced this hit. Only set on a multi_inquire result. */
  inquiry?: number | null;
  details?: {
    category?: string;
    capabilities?: string[];
    phrases?: string[];
    paramTypeRef?: string;
    paramSchema?: Record<string, unknown>;
    contents?: unknown;
  } | null;
}

export interface MultiInquireIntentInquiry {
  kind: string;
  question: string;
  terms: string[];
  prompt?: string | null;
}

export interface MultiInquireIntent {
  mode: "direct" | "inquire";
  response?: string | null;
  memory?: boolean;
  inquiries: MultiInquireIntentInquiry[];
  next_prompt?: string | null;
}

export interface MultiInquireResponse {
  text: string;
  title?: string | null;
  memory: boolean;
  tags: string[];
  citations: string[];
  /** Index of the inquiry that produced this response, or null for the intent phase's own direct answer. */
  inquiry: number | null;
}

export interface ScriptStep {
  action_ref: string;
  params: Record<string, unknown>;
  capture?: string | null;
}

export interface MultiInquireScript {
  title?: string | null;
  actions: string[];
  destructive: string[];
  notes: string[];
  steps: ScriptStep[];
}

export interface MultiInquireResult {
  instruction: string;
  model: string;
  session: string;
  intent: MultiInquireIntent;
  responses: MultiInquireResponse[];
  memories: unknown[];
  scripts: MultiInquireScript[];
  next_prompt?: string | null;
  hits: InquireHit[];
  notes: string[];
  errors: unknown[];
  /**
   * The updated session document multi_inquire assembled from this turn —
   * never saved by multi_inquire itself, so the caller decides whether and
   * when to persist it. See `session.ts`'s `saveSessionDocument`.
   */
  session_document?: XPromptSessionDocument | null;
}

/**
 * One turn as stored inside a session document's `contents.turns[]` —
 * mirrors solx-inquiry's own `multi::run` turn shape (see `session.rs`).
 * Deliberately narrower than `MultiInquireResult`: no `hits` (session
 * history is orientation, not evidence — solx-inquiry never stores them)
 * and no per-turn timestamp or model (neither is written by multi_inquire).
 */
export interface StoredMultiInquireTurn {
  instruction: string;
  mode: "direct" | "inquire";
  inquiries: MultiInquireIntentInquiry[];
  responses: MultiInquireResponse[];
  scripts: MultiInquireScript[];
  memories: unknown[];
  next_prompt?: string | null;
  notes: string[];
  errors: unknown[];
}

/** The document `multi_inquire` returns as `session_document` (see solx-inquiry's `session.rs`). */
export interface XPromptSessionDocument {
  path: string;
  name: string;
  typeRef: string;
  author?: string;
  title?: string;
  summary?: string;
  contents: {
    turns: StoredMultiInquireTurn[];
    turnCount: number;
    lastInstruction: string;
  };
}

/** One row in the session picker — enough to label a dropdown option. */
export interface XPromptSessionSummary {
  name: string;
  title?: string;
  summary?: string;
  updatedAt?: string;
}

/** One turn in the chat transcript. Rendered as a card in the thread. */
export type Turn =
  | { kind: "user"; text: string; at: string }
  | { kind: "answer"; model: string; result: MultiInquireResult; at: string }
  | { kind: "run"; status: "ok" | "error"; message: string; at: string }
  | { kind: "error"; message: string; at: string };

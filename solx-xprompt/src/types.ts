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
  title: string;
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
}

/** One turn in the chat transcript. Rendered as a card in the thread. */
export type Turn =
  | { kind: "user"; text: string; at: string }
  | { kind: "answer"; model: string; result: MultiInquireResult; at: string }
  | { kind: "run"; status: "ok" | "error"; message: string; at: string }
  | { kind: "error"; message: string; at: string };

/**
 * Shapes that cross the widget boundary. These mirror the documented
 * inquire / ollama result schemas (solx-server never validates result
 * shapes, so these are structural only — fields are read defensively).
 */

export interface OllamaModel {
  name: string;
  size?: number;
  capabilities?: string[];
}

export interface OllamaChatResult {
  message?: { role?: string; content?: string; thinking?: string };
  done?: boolean;
  done_reason?: string;
}

export interface InquireHit {
  source: "document" | "action";
  path: string;
  name: string;
  title?: string | null;
  summary?: string | null;
  score?: number;
  matched_terms?: string[];
  details?: {
    category?: string;
    capabilities?: string[];
    phrases?: string[];
    paramTypeRef?: string;
    paramSchema?: Record<string, unknown>;
    contents?: unknown;
  } | null;
}

export interface InquireResult {
  inquiry: string;
  model: string;
  scope: string;
  terms: string[];
  hits: InquireHit[];
  summary: string;
}

/** One turn in the chat transcript. Rendered as a card in the thread. */
export type Turn =
  | { kind: "user"; text: string; at: string }
  | { kind: "chat"; text: string; model: string; at: string }
  | { kind: "inquire"; inquiry: string; model: string; result: InquireResult; at: string }
  | { kind: "error"; message: string; at: string };

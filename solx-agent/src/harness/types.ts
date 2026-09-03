/**
 * The harness's own types. Unlike solx-chat's `conductor/types.ts`, these are
 * not a hand-written mirror of a wasm component's schemas -- the harness runs
 * here, so this is the definition rather than a copy of one.
 *
 * Field names stay snake_case: the session document these describe is written
 * to the solx document store and read back by `solx search`/the CLI, and
 * renaming the stored shape would only make the two views disagree.
 */

/** One entry in a default-deny allowlist. `actions` narrows a path to named actions. */
export interface AllowEntry {
  path: string;
  actions?: string[] | null;
}

/** A context entry either names one document, or searches for several. */
export interface ContextEntry {
  ref?: string | null;
  query?: string | null;
  path?: string | null;
  typeRef?: string | null;
  limit?: number | null;
}

export interface MemorySpec {
  scope: string;
  query: string | null;
  limit: number;
  read: boolean;
  write: boolean;
  max_writes: number;
}

export interface SkillSpec {
  enabled: boolean;
  path: string;
  limit: number;
}

/**
 * `running` and `awaiting_approval` are live; every other status is the
 * user's turn and accepts a new message.
 *
 * `idle` is what the conductor called `final`. The rename is the point: the
 * loop sets it whenever the model replies without calling a tool, which
 * covers "the task is done" *and* "which of these did you mean?", and the UI
 * always rendered it as "your turn". Calling it `final` made the data model
 * disagree with every screen that showed it.
 */
export type SessionStatus =
  | "running"
  | "awaiting_approval"
  | "idle"
  | "blocked"
  | "exhausted"
  | "cancelled";

/** A status is quiescent when nothing is in flight and it is the user's turn. */
export function isQuiescent(status: SessionStatus): boolean {
  return status !== "running" && status !== "awaiting_approval";
}

export interface ToolCall {
  function: { name: string; arguments: Record<string, unknown> };
}

export type Message =
  | { role: "system"; content: string }
  | { role: "user"; content: string }
  | { role: "assistant"; content: string; tool_calls?: ToolCall[] }
  | { role: "tool"; tool_name: string; content: string };

export type Outcome = "ok" | "error" | "refused" | "denied";

/** The audit log, appended in lockstep with the transcript's tool turns. */
export interface CallRecord {
  iteration: number;
  name: string;
  ref: string | null;
  outcome: Outcome;
  approved_by: string | null;
}

/** A call the model wants to make, mid-flight through one turn. */
export interface PendingCall {
  call_id: string;
  name: string;
  ref: string | null;
  arguments: Record<string, unknown>;
  sys: boolean;
  refusal: string | null;
  destructive: boolean;
  result: { outcome: Outcome; content: string } | null;
  approved_by?: string | null;
}

/** What the approval UI is shown: never the stored record, always this view. */
export interface PendingView {
  call_id: string;
  name: string;
  ref: string | null;
  arguments: Record<string, unknown>;
}

export interface ContextIndexEntry {
  ref: string;
  title: string;
  summary: string;
}

export interface ToolDef {
  type: "function";
  function: {
    name: string;
    description: string;
    parameters: Record<string, unknown>;
  };
}

/**
 * The session document's `contents`. One session is one conversation is one
 * document -- there is no transcript anywhere else.
 */
export interface Session {
  id: string;
  model: string;
  /** Titles the document. Taken from the first user turn; never a parameter. */
  title: string;
  status: SessionStatus;

  /** Monotonic across the whole session: the audit trail. */
  iteration: number;
  /** Reset each user turn, and what `max_iterations` is checked against. */
  turn_iteration: number;
  max_iterations: number;
  chat_timeout_secs: number | null;

  /**
   * The security boundary, and the thing the model can never widen. A human
   * can, from the setup panel -- which is recorded as a system turn so the
   * transcript shows when reach changed.
   */
  grant: AllowEntry[];
  /** Re-resolved from the newest user message at the top of every turn. */
  tools: Record<string, string>;
  tools_defs: ToolDef[];
  tools_dropped: number;
  catalogue_cap: number;
  tool_search: boolean;

  memory: MemorySpec | null;
  memories_written: number;
  context: ContextIndexEntry[];
  skills: SkillSpec;
  skills_seen: Record<string, boolean>;

  messages: Message[];
  calls: CallRecord[];
  pending: PendingCall[];
  consecutive_failures: number;

  /** The model's last reply when it yielded the turn. */
  answer: string | null;
}

/** What a step reports back to the caller driving the loop. */
export interface StepResult {
  session_id: string;
  status: SessionStatus;
  iteration: number;
  turn_iteration: number;
  max_iterations: number;
  answer: string | null;
  pending: PendingView[];
  tools: string[];
  tools_dropped: number;
  memories_written: number;
}

export interface CatalogueToolDef {
  function: { name: string; description?: string };
}

/** What the setup panel's catalogue preview shows. */
export interface ToolsPreview {
  tools: CatalogueToolDef[];
  refs: Record<string, string>;
  dropped: number;
}

export interface OllamaModel {
  name: string;
  capabilities?: string[];
}

/**
 * The fixed verb set, the builtins they map to, and the budget constants.
 *
 * This file is the whole reason solx-inquiry exists. solx-agent resolves a
 * tool catalogue per turn and hands the model wire tool names it may call;
 * here the model is handed exactly five verbs and the harness owns the
 * mapping to real actions. That is the "optimize for the solx entities"
 * trade: the model never sees `act__…`, never needs `sys__tool_search`, and
 * never gets a call refused, because the set it can ask for is closed and
 * every member is read-only.
 *
 * Wire spelling matters and is not uniform across solx -- the same rule
 * `solx-agent/src/harness/refs.ts` documents at length. Every parameter here
 * is read by a `#[serde(rename_all = "camelCase")]` struct (`SearchQuery`,
 * `ListOptions`, `ActionSearchQuery`, `EntityRefParams`), so camelCase is
 * correct throughout: `pathPrefix`, `typeRef`, `limit`. The one snake_case
 * name in sight is the *verb's own* vocabulary (`path_prefix`), which never
 * crosses the wire -- `execute.ts` translates it.
 */

export const CHAT = "/packages/solx-ollama/ollama-chat";
export const LIST_MODELS = "/packages/solx-ollama/ollama-list-models";

/** Document read/write builtins, for the inquiry's own session documents. */
export const SAVE_DOC = "/builtin/document/entity-save-document";
export const GET_DOC = "/builtin/document/entity-get-document";

/** The read-only builtins the verbs resolve to. Verified against solx-core's seed catalogue. */
export const SEARCH_DOCS = "/builtin/document/search-documents";
export const LIST_DOCS = "/builtin/document/entity-list-documents";
export const LIST_DOC_PATHS = "/builtin/document/entity-list-document-paths";
export const LIST_TYPES = "/builtin/type/entity-list-types";
export const SEARCH_ACTIONS = "/builtin/action/search-actions";

/** Document deletion, for removing the inquiry's own session document. */
export const DELETE_DOC = "/builtin/document/entity-delete-document";

/**
 * Where every document this package owns lives, and its one type.
 *
 * A single root means the package footprint is one query (`solx search --path
 * /inquiry`) and there is no gate to reserve a prefix for -- inquiry writes
 * nothing but its own sessions.
 */
export const INQUIRY_ROOT = "/inquiry";
export const SESSION_PATH = INQUIRY_ROOT + "/sessions";
export const SESSION_TYPE = "/packages/solx-inquiry/InquirySession";

/**
 * Verb -> builtin. The only place a verb becomes an action reference.
 *
 * `list` and `search` are the same builtin because a no-`q` search *is* a
 * list; they are separate verbs only so the planner can say what it means,
 * and so the transcript reads clearly.
 */
export const VERB_ACTION: Record<string, string> = {
  search: SEARCH_DOCS,
  list: SEARCH_DOCS,
  get: GET_DOC,
  types: LIST_TYPES,
  actions: SEARCH_ACTIONS,
};

export const VERBS: string[] = Object.keys(VERB_ACTION);

/**
 * A per-step result cap. The planner may ask for more; the harness clamps.
 * Small on purpose -- this is a grounding set for one answer, not a bulk
 * export, and every document is prompt tokens in the answer call.
 */
export const MAX_STEP_LIMIT = 25;
export const DEFAULT_STEP_LIMIT = 10;

export const MAX_PLAN_STEPS_HARD = 8;
export const DEFAULT_MAX_PLAN_STEPS = 4;

export const DEFAULT_MAX_DOCS = 12;
export const MAX_DOCS_HARD = 30;

/**
 * Characters of document text handed to the answer phase, total.
 *
 * The answer call is where a small local model drowns, so this is the number
 * that decides how many documents can actually be used rather than merely
 * gathered. Twelve documents of 1,500 characters each is already 18k
 * characters of transcript before the question.
 */
export const DEFAULT_DOC_BUDGET = 18000;
export const MAX_DOC_BUDGET = 60000;

/** Per-document slice taken before the budget is applied. */
export const DOC_EXCERPT_CAP = 3000;

/**
 * How many distinct document paths the corpus index lists, and how many types.
 * Each is one capped list call; the index is injected into the planner prompt,
 * so it is bounded by prompt budget, not by how large the store is.
 */
export const CORPUS_PATH_LIMIT = 40;
export const CORPUS_TYPE_LIMIT = 20;
/** How many of the most populated paths are sampled for their document type. */
export const CORPUS_TYPE_SAMPLE_PATHS = 8;
/** Characters of the rendered corpus index, so a huge store cannot flood the planner. */
export const CORPUS_INDEX_CAP = 2400;

/**
 * How many type rows to fetch before filtering client-side.
 *
 * There is no `search-types` builtin -- `entity-list-types` takes a plain
 * `ListOptions` with no `q` -- so a keyword search over types is a generous
 * fetch filtered in the harness. This is larger than the answer's `limit`
 * because the filtering happens after the fetch, and a registry is small.
 */
export const TYPE_FETCH = 60;

/** How many prior turns of the conversation the planner and answer phases see. */
export const HISTORY_TURNS = 4;
/** Characters of a prior question/answer kept when history is included. */
export const HISTORY_TEXT_CAP = 600;

/** Consecutive phases that may fail before a turn is marked `error`. */
export const MAX_PHASE_FAILURES = 2;

/** Split an action ref into its `(path, name)` halves. `path` is `"/"` at the root. */
export function splitRef(ref: string): { path: string; name: string } {
  const slash = ref.lastIndexOf("/");
  return { path: ref.slice(0, slash) || "/", name: ref.slice(slash + 1) };
}

/** Join a path and a name into the one reference spelling the CLI and transcripts use. */
export function refOf(path: string, name: string): string {
  return (path === "/" ? "" : path) + "/" + name;
}

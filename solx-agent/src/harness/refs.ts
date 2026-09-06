/**
 * Action refs, document roots and tuning constants.
 *
 * Wire spelling matters and is not uniform across solx. Anything parsed into
 * a serde struct is camelCase (`ActionSearchQuery`, `SearchQuery`,
 * `DocumentInput`); handlers that read raw keys use snake_case (`rel_path`,
 * `doc_path`). Schemas are open, so a wrong key is *silently dropped* rather
 * than rejected -- which for `excludeHidden` would mean an unfiltered
 * catalogue with no error at all. Every key used here was checked against the
 * handler that reads it.
 */

export const CHAT = "/packages/solx-ollama/ollama-chat";
export const LIST_MODELS = "/packages/solx-ollama/ollama-list-models";
export const SEARCH_ACTIONS = "/builtin/action/search_actions";
export const GET_ACTION = "/builtin/action/entity_get_action";
export const GET_TYPE = "/builtin/type/entity_get_type";
export const SAVE_DOC = "/builtin/document/entity_save_document";
export const GET_DOC = "/builtin/document/entity_get_document";
export const SEARCH_DOCS = "/builtin/document/search_documents";

/**
 * Every document this package owns lives under one root. Grouping by owner
 * rather than by kind is what lets the reserved-path check be a single
 * prefix: a fifth kind of document cannot be added and then forgotten by the
 * gate. It also makes the package footprint one query -- `solx search --path
 * /agent`.
 */
export const AGENT_ROOT = "/agent";
export const SESSION_PATH = AGENT_ROOT + "/sessions";
export const SESSION_TYPE = "/packages/solx-agent/AgentSession";
export const MEMORY_ROOT = AGENT_ROOT + "/memories";
export const MEMORY_TYPE = "/packages/solx-agent/AgentMemory";
export const DEFAULT_SKILLS_PATH = AGENT_ROOT + "/skills";
export const SKILL_TYPE = "/packages/solx-agent/AgentSkill";

/**
 * Every tool definition is prompt tokens on every iteration, so the catalogue
 * is resolved from a task query and then capped. A 4B model drowns long
 * before it runs out of context.
 */
export const DEFAULT_CATALOGUE_CAP = 16;
export const SEARCH_FETCH = 50;

export const DEFAULT_MAX_ITERATIONS = 12;
/** Consecutive iterations where every dispatch failed. */
export const MAX_CONSECUTIVE_FAILURES = 3;

export const MEMORY_TEXT_CAP = 1000;
export const DEFAULT_MEMORY_LIMIT = 5;
export const MAX_MEMORY_LIMIT = 20;
export const DEFAULT_MEMORY_WRITES = 20;

export const DEFAULT_CONTEXT_CAP = 20;
export const CONTEXT_READ_CAP = 20000;

/**
 * How many skill documents to list as candidates. This is a *candidate fetch*,
 * not a filter: `resolveSkills` no longer passes a full-text query, so the
 * globs do all the selecting and this only has to be larger than the number of
 * skills that live under the skills path.
 */
export const SKILL_SEARCH_LIMIT = 40;
/**
 * Prompt budget, and the reason the seeded skills are each kept short. A 4B
 * model drowns long before it runs out of context, so the total is what
 * decides how many can ride along in one turn.
 */
export const SKILL_INSTRUCTIONS_CAP = 4000;
export const SKILL_TOTAL_CAP = 8000;

/**
 * Dangerous because *this* is the caller, which is why they are here and not
 * in the shared exclusion list. No grant reaches past them.
 *
 * `/builtin/action` is denied by exact name rather than by glob, because the
 * read half of that path is exactly what a model needs to understand the
 * system it is working in: the action registry *is* the tool catalogue, so
 * `search_actions` / `entity_get_action` / `entity_list_actions` let it look
 * up a tool it was not handed and read that tool's parameter schema. Those
 * three stay reachable when granted; the six below never are.
 */
export const HARD_DENY = [
  // Secrets resolve against the calling action's own action_config, so
  // exposing them hands the model the keys the harness runs under.
  "/builtin/secrets/*",
  // Detached spawning. Denied because the *widget* drives these to run the
  // loop -- a model reaching them could start, poll and stop its own
  // invocations out from under the operator.
  "/builtin/action/start",
  "/builtin/action/stop",
  "/builtin/action/poll",
  "/builtin/action/cancelled",
  // Self-modification. Note this is *not* about command and webhook rows:
  // solx-core's `guard_executable_action` already refuses those through
  // `entity_save_action` for every caller that is an action, an MCP tool
  // call, or a script. The reason they are denied here is that a `script` or
  // `wasm` row registered under an already-granted path is a full gate
  // bypass -- a guest's `action-exec` import reaches anything, and a .solx
  // script composes anything, neither of them filtered by this session's
  // grant.
  "/builtin/action/entity_save_action",
  "/builtin/action/entity_delete_action",
  // Persists through to solx-config.json.
  "/builtin/env/set_env",
  // No recursive self-invocation.
  "/packages/solx-agent/*",
];

/**
 * Document writers, mapped to the param naming their *entity* path.
 *
 * The trap: `set_field_at_path` takes the entity path as `doc_path`, while
 * its `path` is a JSON pointer into contents. Reading the wrong key here
 * would silently disable the check for exactly the call that can rewrite one
 * field of a session document.
 */
export const DOC_WRITERS: Record<string, string> = {
  "/builtin/document/entity_save_document": "path",
  "/builtin/document/entity_delete_document": "path",
  "/builtin/document/set_field": "path",
  "/builtin/document/set_field_at_path": "doc_path",
};

/**
 * Built-in tools are handled in this package and backed by no action row, so
 * they sidestep the HARD_DENY on its own path. The prefix cannot collide with
 * a catalogue name, which is always `act__...`.
 */
export const SYS_PREFIX = "sys__";
export const SYS_MEMORY_SAVE = SYS_PREFIX + "memory_save";
export const SYS_MEMORY_SEARCH = SYS_PREFIX + "memory_search";
export const SYS_CONTEXT_READ = SYS_PREFIX + "context_read";
export const SYS_TOOL_SEARCH = SYS_PREFIX + "tool_search";

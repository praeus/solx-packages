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

export const SKILL_SEARCH_LIMIT = 10;
export const SKILL_INSTRUCTIONS_CAP = 4000;
export const SKILL_TOTAL_CAP = 8000;

/**
 * Dangerous because *this* is the caller, which is why they are here and not
 * in the shared exclusion list. No grant reaches past them.
 */
export const HARD_DENY = [
  // Secrets resolve against the calling action's own action_config, so
  // exposing them hands the model the keys the harness runs under.
  "/builtin/secrets/*",
  // entity_save_action, entity_delete_action, action start/stop/poll:
  // self-modification and detached spawning. This matters *more* now that
  // the loop is browser-driven, because the widget itself drives
  // /builtin/action/{start,poll,stop} and the model must never reach them.
  "/builtin/action/*",
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

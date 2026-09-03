/**
 * The agent harness: an agent loop over the solx action catalogue, running in
 * the page rather than behind an action.
 *
 * The action registry *is* a tool catalogue -- every row carries a name, a
 * description, and a `paramTypeRef` pointing at a JSON Schema, which is what
 * Ollama's tool format wants. So this resolves a catalogue from a search
 * query, hands it to a model through `solx-ollama`, and dispatches whatever
 * the model asks for back into solx.
 *
 * Everything it knows lives in the document store under `/agent`, so a
 * session is authored, searched, edited and inspected with the tools solx
 * already has. There is no private storage anywhere in this package.
 *
 * Module map, roughly in dependency order:
 *
 * - `host`      the single `exec` seam everything below talks through
 * - `refs`      action refs, document roots, tuning constants
 * - `gate`      the allowlist, structural denies, reserved paths
 * - `catalogue` resolving a tool list from a query and a grant
 * - `knowledge` memories, context documents, skills
 * - `session`   the session document, its name, and call ids
 * - `sysTools`  the four built-in `sys__` tools
 * - `turn`      one iteration: chat, gate, dispatch, flush
 * - `agent`     sessions and conversation turns
 * - `loop`      driving a session to a quiescent state
 */

export { hostFromClient } from "./host";
export type { ExecClient, Host } from "./host";

export { addTurn, createSession, previewTools, widenGrant } from "./agent";
export type { SendOptions } from "./agent";

export { approveAndContinue, driveSession, readSession } from "./loop";
export type { DriveHandlers, DriveOptions } from "./loop";

export { step, summarize } from "./turn";
export { loadSession, saveSession } from "./session";
export { normalizeAllow, permitted } from "./gate";
export { LIST_MODELS, SEARCH_DOCS, SESSION_PATH, SESSION_TYPE } from "./refs";

export * from "./types";

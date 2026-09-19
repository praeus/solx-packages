/**
 * Action references this widget calls. Centralized so the strings don't
 * drift between the call sites and the install metadata (which lists the
 * same packages as action phrases).
 *
 * multi_inquire is the only research/chat action this widget calls: its own
 * intent phase decides whether an instruction can be answered directly or
 * needs inquiries fanned out (see solx-inquiry's intent.rs), so there is no
 * separate ollama-chat call or keyword-based routing here to duplicate that
 * decision — see dispatch.ts.
 */

export const LIST_MODELS = "/packages/solx-ollama/ollama-list-models";

export const INQUIRY_PATH = "/packages/solx-inquiry";
export const MULTI_INQUIRE_FN = "multi-inquire";

export const RANDOM_NAME = "/packages/solx-names/random-name";

/** Where this widget's session documents live — multi_inquire's own `session` ref, persisted here after every turn. */
export const XPROMPT_SESSION_PATH = "/xprompt/sessions";

export const SAVE_DOC = "/builtin/document/entity-save-document";
export const GET_DOC = "/builtin/document/entity-get-document";
export const LIST_DOCS = "/builtin/document/entity-list-documents";
export const DELETE_DOC = "/builtin/document/entity-delete-document";

export const SEARCH_ACTIONS = "/builtin/action/search-actions";
export const SEARCH_DOCS = "/builtin/document/search-documents";

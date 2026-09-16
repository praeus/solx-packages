/**
 * Action references this widget calls. Centralized so the strings don't
 * drift between the call sites and the install metadata (which lists the
 * same packages as action phrases).
 *
 * Inquiry and Ollama are separate packages by design — inquire is the one
 * that decides what to search and how to summarize the result, ollama is
 * one of several possible chat backends. The widget defaults to whatever
 * `inquire` defaults to (which defaults to ollama-chat), and only talks to
 * ollama directly when the user wants a plain conversational turn with no
 * search.
 */

export const LIST_MODELS = "/packages/solx-ollama/ollama-list-models";
export const OLLAMA_CHAT = "/packages/solx-ollama/ollama-chat";
export const INQUIRE = "/packages/solx-inquiry/inquire";

export const SAVE_DOC = "/builtin/document/entity-save-document";
export const GET_DOC = "/builtin/document/entity-get-document";

export const SEARCH_ACTIONS = "/builtin/action/search-actions";
export const SEARCH_DOCS = "/builtin/document/search-documents";

/**
 * Heuristic: a prompt is treated as "research" rather than "chat" if it
 * contains a question word or any of these triggers. Kept inline so the
 * routing decision is obvious in the dispatch code; expanded as the widget
 * learns more routing (e.g. an explicit "mode" field).
 */
export const RESEARCH_TRIGGERS = [
  "search",
  "find",
  "look up",
  "what do i have",
  "what do i know",
  "summarize",
  "research",
  "investigate",
  "show me",
];

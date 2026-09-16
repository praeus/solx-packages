export const SAVE_DOC = "/builtin/document/entity_save_document";
export const GET_DOC = "/builtin/document/entity_get_document";

/** Every document this package owns lives under one root, mirroring solx-agent's `/agent`. */
export const XPROMPT_ROOT = "/xprompt";
export const CALL_LOG_PATH = XPROMPT_ROOT + "/call-logs";
export const CALL_LOG_TYPE = "/packages/solx-xprompt/XPromptCallLog";

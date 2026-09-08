//! Default prompts for the two LLM phases, and the JSON schema used to force
//! structured output out of the search-term phase.
//!
//! Both defaults are deliberately short: this pipeline targets small local
//! models via Ollama, and a long system prompt eats into the context budget
//! those models can least afford to spend.

use serde_json::{json, Value};

pub const DEFAULT_INQUIRY_PROMPT: &str = "\
You turn a user's inquiry into search terms for a full-text index over \
documents and actions. Each term is matched as a single query where every \
word in it must appear together in the same result, so a multi-word term is \
narrower, not broader. Read the inquiry and respond with distinct single \
keywords (one word each; two only when the concept genuinely has no \
one-word form, e.g. a proper name) most likely to find material relevant to \
answering it. Prefer several narrow single-word terms over one long phrase.";

pub const DEFAULT_SUMMARY_PROMPT: &str = "\
You answer a user's inquiry using only the search results supplied below. \
Write a concise, direct answer grounded in those results, citing each one \
you rely on by its path. If the results do not answer the inquiry, say so \
plainly instead of guessing.";

/// JSON schema passed as the chat action's `format`, forcing the model to
/// answer with `{"terms": [...]}` rather than free text that would need
/// best-effort parsing.
pub fn terms_schema(max_terms: usize) -> Value {
    json!({
        "type": "object",
        "properties": {
            "terms": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "maxItems": max_terms
            }
        },
        "required": ["terms"]
    })
}

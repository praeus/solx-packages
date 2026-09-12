//! Default prompts for the LLM phases, and the JSON schemas used to force
//! structured output out of each of them.
//!
//! Every default is deliberately short: this pipeline targets small local
//! models via Ollama, and a long system prompt eats into the context budget
//! those models can least afford to spend. That bites hardest on `instruct`'s
//! inquiry prompts, which also carry skills, memories and a page of search
//! hits.

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

// ── instruct ────────────────────────────────────────────────────────────────
//
// Three prompts, one per decision the `instruct` pipeline asks a model to
// make: what the instruction needs (intent), what the documents say (document
// inquiry), and what to run (action inquiry). Each is paired with a `format`
// schema below, so a compliant model answers in a shape that parses rather
// than in prose that has to be guessed at.

pub const DEFAULT_INTENT_PROMPT: &str = "\
You decide what a user's instruction needs before any work is done. Answer \
with mode \"direct\" when you can respond from the instruction and the \
reference material already given to you, and put that answer in \"response\". \
Answer with mode \"inquire\" when the instruction needs material you do not \
have, and list what to look up. A \"documents\" inquiry searches stored notes \
and other written material; anything already given to you above as reference \
material will not come back from one, so never propose an inquiry to look that \
up again. An \"actions\" inquiry searches the catalogue of things \
this system can run, and is what you pick when the instruction asks for \
something to be done rather than explained. Give every inquiry a question and \
its own search terms: distinct single keywords, because every word in one \
term must appear together in the same result. Use \"prompt\" only to add an \
instruction-specific note for that one inquiry. Set \"memory\" true on a \
direct response only when it states something durable that would be worth \
knowing in a later, unrelated session. Set \"next_prompt\" only when the \
instruction clearly needs a separate, later round after this one - for \
example, once whatever you looked up or asked to run has been acted on. Word \
it as the instruction for that later round, not a description of it. Leave it \
out for anything answerable in this one call.";

pub const DEFAULT_DOCUMENT_INQUIRY_PROMPT: &str = "\
You answer one question using only the search results supplied below. Write \
each distinct finding as its own response, grounded in those results. In \
\"citations\", name the results you relied on by the full path shown for each \
one, such as /notes/auth - never by its number in the list, which means \
nothing outside this message. If the results do not answer the question, say \
so plainly in a single response instead of guessing. Set \"memory\" true only on a response that states something \
durable and self-contained - a fact that would still be worth knowing in a \
later, unrelated session - never on a restatement of the question, and never \
on something true only of this run.";

pub const DEFAULT_ACTION_INQUIRY_PROMPT: &str = "\
You turn one question into runnable work, using only the actions listed \
below. Answer with scripts, each a short ordered list of steps. Every step \
names an action by the exact reference given in the results - never invent \
one, and never name an action that is not listed - and supplies its \
parameters as an object satisfying that action's parameter schema, which is \
included with it. Omit an optional parameter rather than passing null. Use \
\"capture\" to name a step's result when a later step needs it. If the listed \
actions cannot do what was asked, return no scripts and say why in \"notes\" \
rather than improvising an action that does not exist. An action whose \
capabilities include solx:destructive changes or removes something: choose \
one only when the instruction actually asked for that, and never merely to \
inspect, list or read something.";

/// What the model is authoring *for*, given it never writes `.solx` itself.
///
/// A step list is only sensible if you know it becomes a pipeline of `exec`
/// stages whose captured values later steps can read, and that the last one
/// decides the result. Those, plus the two constraints that actually bind (no
/// control flow, no return), are all this needs to say.
///
/// The full `.solx` grammar - quoting, `;` rules, textual substitution - is
/// deliberately absent, because nothing the model produces passes through it:
/// [`crate::script`] owns every character of syntax, which is the whole reason
/// the model emits steps instead of text. The complete primer is seeded as a
/// skill document at `/solx-inquiry/skills/solx-scripts`, for a human reading
/// a returned script and for an action inquiry that recall pulls it into.
pub const SOLX_SCRIPT_PRIMER: &str = "\
Your steps become a .solx script: a sequence of statements, each one action \
call, run in the order you give. A step's \"capture\" name makes its result \
readable by later steps as $name, or $name.field for one field of it. A \
script runs as an action, which supports only action calls - no conditionals, \
no loops, and no return, because the last step's result is the script's \
result. So order the steps so the one whose result answers the question comes \
last. You do not write .solx syntax yourself: write the steps and their \
parameters, and the syntax is generated for you.";

/// Forces the intent phase to answer with a parseable decision rather than
/// prose.
///
/// `terms` is `required` on every inquiry precisely so the inquiry phase never
/// has to spend an llm call of its own generating them - that call would not
/// only double the budget, it would serialize the fan-out those calls are
/// parallelized to avoid. See [`crate::terms::fallback_terms`] for what
/// happens when a model ignores this anyway.
pub fn intent_schema(max_inquiries: usize, max_terms: usize) -> Value {
    json!({
        "type": "object",
        "properties": {
            "mode": { "type": "string", "enum": ["direct", "inquire"] },
            "response": { "type": "string" },
            "memory": { "type": "boolean" },
            "inquiries": {
                "type": "array",
                "maxItems": max_inquiries,
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["documents", "actions"] },
                        "question": { "type": "string" },
                        "terms": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "maxItems": max_terms
                        },
                        "prompt": { "type": "string" }
                    },
                    "required": ["kind", "question", "terms"]
                }
            },
            "next_prompt": { "type": "string" }
        },
        "required": ["mode"]
    })
}

/// The document-inquiry answer shape: several findings, each independently
/// citable and independently flaggable as worth remembering.
pub fn responses_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "responses": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "title": { "type": "string" },
                        "memory": { "type": "boolean" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "citations": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["text"]
                }
            }
        },
        "required": ["responses"]
    })
}

/// The action-inquiry answer shape: structured steps, never `.solx` text.
///
/// `params` is left unconstrained here on purpose. The real constraint is the
/// called action's own `paramSchema`, which sits in the prompt beside it and
/// which the host validates on every `exec` regardless of what this says -
/// restating it here would only be a second, drifting copy.
pub fn scripts_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "scripts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "notes": { "type": "string" },
                        "steps": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "action_ref": { "type": "string" },
                                    "params": { "type": "object" },
                                    "capture": { "type": "string" }
                                },
                                "required": ["action_ref", "params"]
                            }
                        }
                    },
                    "required": ["steps"]
                }
            }
        },
        "required": ["scripts"]
    })
}

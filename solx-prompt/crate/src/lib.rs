//! solx-prompt — one `wasm32-wasip2` component backing the `prompt` action:
//! take a request and turn it into a message plus a runnable plan of action
//! steps.
//!
//! Three phases, two model calls, no parallelism:
//!
//! ```text
//! baseline recall (skills + memories)   no llm
//! context documents                     no llm
//! history block (from params)           no llm
//! intent                                1 llm
//!   no steps and no searches -> return the message and stop
//! search                                no llm
//! steps                                 1 llm
//! ```
//!
//! Every phase is an ordinary nested `exec` call — this component has no
//! capability the built-in catalogue and an installed chat action (by default
//! `solx-ollama`'s `ollama-chat`) don't already provide.
//!
//! **This action saves nothing.** History arrives inline as a parameter and
//! this turn's record goes back out; memories come back as ready-to-save
//! payloads and steps as a ready-to-execute plan. Deciding what to keep, run or
//! persist is entirely the caller's, which is what keeps the thing that
//! produces model output from also being the thing that acts on it or records
//! it. There is no `entity-save-document` reference anywhere in this crate, and
//! a test asserts as much.

// Copied from solx-inquiry and typed on this package's own params - see the
// README on why these are copies rather than a shared crate. Wired in here but
// not yet called by `run`: the phases that use them land next.
pub mod console;
pub mod host;
pub mod llm;
pub mod params;
pub mod parse;
pub mod script;
pub mod search;
pub mod terms;

#[cfg(target_arch = "wasm32")]
mod guest;

use serde_json::{json, Value};

use host::{normalize_params, str_param, Host, Outcome};

pub const PROMPT_FN: &str = "prompt";

pub fn dispatch(host: &dyn Host, fn_name: Option<&str>, params_json: &str) -> Outcome {
    let Some(fn_name) = fn_name else {
        return Outcome::fail(
            "unknown_action",
            "the action row has no fn_name; solx-prompt dispatches on fn_name",
            json!({ "known": [PROMPT_FN] }),
        );
    };

    let params: Value = match serde_json::from_str(params_json) {
        Ok(v @ Value::Object(_)) => v,
        // An empty params string is how a caller spells "no arguments".
        Ok(Value::Null) => Value::Object(serde_json::Map::new()),
        Ok(_) => {
            return Outcome::fail("bad_params", "params must be a JSON object", json!({}));
        }
        Err(e) => {
            return Outcome::fail(
                "bad_params",
                format!("params are not valid JSON: {e}"),
                json!({}),
            );
        }
    };

    match fn_name {
        PROMPT_FN => run(host, &params),
        other => Outcome::fail(
            "unknown_action",
            format!("unknown fn_name {other}"),
            json!({ "fn_name": other, "known": [PROMPT_FN] }),
        ),
    }
}

/// The three-phase pipeline.
///
/// Scaffold: this returns the real result envelope with every phase's
/// contribution empty, so the shape a caller and the widget code against is
/// fixed before any phase exists. Filling the phases in replaces the body and
/// must not change the envelope.
fn run(_host: &dyn Host, params: &Value) -> Outcome {
    let params = normalize_params(params);

    let Some(prompt) = str_param(&params, "prompt") else {
        return Outcome::fail("bad_params", "prompt is required and must be non-empty", json!({}));
    };
    let Some(model) = str_param(&params, "model") else {
        return Outcome::fail("bad_params", "model is required and must be non-empty", json!({}));
    };

    let message = String::new();
    let next_prompt: Option<String> = None;

    Outcome::ok(json!({
        "prompt": prompt,
        "model": model,
        // "direct" once the intent phase answers without searching, "steps"
        // once it proposes work. Read off the content, never off a declared
        // mode - see the plan's note on `intent::parse`.
        "mode": "direct",
        "message": message,
        "steps": [],
        "destructive": [],
        "next_prompt": next_prompt,
        "memories": [],
        // This turn's record, shaped for a session document's `contents.turns[]`.
        // It deliberately carries no `results`: the steps have not run yet, and
        // the caller that runs them is the one that appends them.
        "turn": {
            "prompt": prompt,
            "message": message,
            "steps": [],
            "next_prompt": next_prompt,
            "memories": [],
            "notes": [],
            "errors": [],
        },
        "hits": [],
        "notes": [],
        "errors": [],
    }))
}

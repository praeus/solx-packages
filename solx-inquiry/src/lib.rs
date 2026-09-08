//! solx-inquiry — one `wasm32-wasip2` component backing the `inquire`
//! action: take a question, ask an LLM action for search terms, search
//! documents and/or actions with those terms, then ask the LLM action to
//! summarize the results against the original question.
//!
//! The three phases are ordinary nested `exec` calls — this component has no
//! capability the built-in catalogue and an installed chat action (by
//! default `solx-ollama`'s `ollama-chat`) don't already provide. See
//! README.md for why that pipeline shape belongs in wasm: it's a single
//! straight-line sequence with no interactive loop, which is exactly what one
//! `exec`-only guest invocation is good for.

pub mod host;
pub mod llm;
pub mod params;
pub mod prompts;
pub mod search;
pub mod summarize;
pub mod terms;

#[cfg(target_arch = "wasm32")]
mod guest;

use serde_json::{json, Value};

use host::{Host, Outcome};

pub const INQUIRE_FN: &str = "inquire";

pub fn dispatch(host: &dyn Host, fn_name: Option<&str>, params_json: &str) -> Outcome {
    let Some(fn_name) = fn_name else {
        return Outcome::fail(
            "unknown_action",
            "the action row has no fn_name; solx-inquiry dispatches on fn_name",
            json!({ "known": [INQUIRE_FN] }),
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
        INQUIRE_FN => inquire(host, &params),
        other => Outcome::fail(
            "unknown_action",
            format!("unknown fn_name {other}"),
            json!({ "fn_name": other, "known": [INQUIRE_FN] }),
        ),
    }
}

fn inquire(host: &dyn Host, params: &Value) -> Outcome {
    let p = match params::parse(params) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };

    let terms = match terms::generate_search_terms(host, &p) {
        Ok(t) => t,
        Err(outcome) => return outcome,
    };

    let hits = match search::run_search(host, &p, &terms) {
        Ok(h) => h,
        Err(outcome) => return outcome,
    };

    let summary = match summarize::summarize(host, &p, &hits) {
        Ok(s) => s,
        Err(outcome) => return outcome,
    };

    Outcome::ok(json!({
        "inquiry": p.inquiry,
        "model": p.model,
        "scope": p.scope.as_str(),
        "terms": terms,
        "hits": hits.iter().map(search::Hit::to_json).collect::<Vec<_>>(),
        "summary": summary,
    }))
}

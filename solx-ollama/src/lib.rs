//! solx-ollama — one `wasm32-wasip2` component backing every Ollama action.
//!
//! The host passes each action row's `fn_name` through as the guest's
//! `action-name` argument, so a single artifact can serve N registered
//! actions. [`dispatch`] is the entry point; `guest.rs` is a thin wit-bindgen
//! shim around it.
//!
//! All outbound HTTP goes through `/builtin/http_request` — the guest has no
//! sockets of its own (WASI is stubbed by the host).

pub mod config;
pub mod endpoint;
pub mod host;
pub mod request;

#[cfg(target_arch = "wasm32")]
mod guest;

use serde_json::{json, Value};

use endpoint::SET_API_KEY_FN;
use host::{Host, Outcome};

pub fn dispatch(host: &dyn Host, fn_name: Option<&str>, params_json: &str) -> Outcome {
    let Some(fn_name) = fn_name else {
        return Outcome::fail(
            "unknown_action",
            "the action row has no fn_name; solx-ollama dispatches on fn_name",
            json!({ "known": endpoint::known_fn_names() }),
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

    if fn_name == SET_API_KEY_FN {
        return set_api_key(host, &params);
    }

    match endpoint::lookup(fn_name) {
        Some(ep) => request::call(host, ep, &params),
        None => Outcome::fail(
            "unknown_action",
            format!("unknown fn_name {fn_name}"),
            json!({ "fn_name": fn_name, "known": endpoint::known_fn_names() }),
        ),
    }
}

/// Store the bearer token under [`config::DEFAULT_SECRET`].
///
/// This exists to break a chicken-and-egg problem: `/builtin/set_secret`
/// encrypts with a key drawn from the *calling* action's
/// `action_config.secrets`, so only an action that already carries the key can
/// write the secret. The name is hardcoded, so this action can never overwrite
/// an unrelated secret.
fn set_api_key(host: &dyn Host, params: &Value) -> Outcome {
    let Some(value) = host::str_param(params, "value") else {
        return Outcome::fail(
            "bad_params",
            "set_api_key requires a non-empty value",
            json!({ "missing": ["value"] }),
        );
    };

    let payload = json!({ "name": config::DEFAULT_SECRET, "value": value });
    match host.exec("/builtin/set_secret", &payload) {
        Ok(c) if c.success => Outcome::ok(json!({
            "set": true,
            "name": config::DEFAULT_SECRET,
        })),
        Ok(c) => Outcome::fail(
            "auth",
            c.message.unwrap_or_else(|| "set_secret failed".to_string()),
            json!({ "name": config::DEFAULT_SECRET }),
        ),
        Err(e) => Outcome::fail("auth", e, json!({ "name": config::DEFAULT_SECRET })),
    }
}

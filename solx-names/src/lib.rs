//! solx-names — a single `wasm32-wasip2` component backing the `random-name`
//! action.
//!
//! Returns a random adjective-noun name from the `names` crate. Passing
//! `with_id: true` appends an 8-hex-digit id, e.g. `capable-tiger-9f2c1a06`.
//! That id is 4 random bytes hex-encoded — the same shape as the first group
//! of a v4 GUID (`9f2c1a06` in `9f2c1a06-....-....-....-............`), since
//! that first group carries no version/variant bits and so is itself just 32
//! random bits.
//!
//! No host calls are needed for this: unlike solx-ollama/solx-inquiry, there
//! is no `Host` trait here, just a pure `dispatch` that `guest.rs` (the
//! wit-bindgen shim, wasm32-only so `cargo test` can still link on the host
//! target) calls straight through to.

#[cfg(target_arch = "wasm32")]
mod guest;

use names::Generator;
use rand::RngCore;
use serde_json::{json, Value};

const FN_NAME: &str = "random_name";

/// Bindgen-free mirror of the WIT `action-result` record.
pub struct Outcome {
    pub success: bool,
    pub message: Option<String>,
    pub output: Value,
}

impl Outcome {
    pub fn ok(output: Value) -> Self {
        Outcome { success: true, message: None, output }
    }

    /// Build a failure whose `output` is a machine-readable object.
    ///
    /// `extra` must be a JSON object; `kind` and `error` are merged into it.
    pub fn fail(kind: &str, message: impl Into<String>, mut extra: Value) -> Self {
        let message = message.into();
        if !extra.is_object() {
            extra = Value::Object(serde_json::Map::new());
        }
        if let Some(o) = extra.as_object_mut() {
            o.insert("kind".into(), Value::String(kind.into()));
            o.insert("error".into(), Value::String(message.clone()));
        }
        Outcome { success: false, message: Some(message), output: extra }
    }
}

pub fn dispatch(fn_name: Option<&str>, params_json: &str) -> Outcome {
    let Some(fn_name) = fn_name else {
        return Outcome::fail(
            "unknown_action",
            "the action row has no fn_name; solx-names dispatches on fn_name",
            json!({ "known": [FN_NAME] }),
        );
    };
    if fn_name != FN_NAME {
        return Outcome::fail(
            "unknown_action",
            format!("unknown fn_name {fn_name}"),
            json!({ "fn_name": fn_name, "known": [FN_NAME] }),
        );
    }

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

    let with_id = params.get("with_id").and_then(Value::as_bool).unwrap_or(false);
    Outcome::ok(json!({ "name": random_name(with_id) }))
}

/// One adjective-noun name, e.g. `capable-tiger`; with `with_id`,
/// `capable-tiger-9f2c1a06`.
fn random_name(with_id: bool) -> String {
    let name = Generator::default()
        .next()
        .unwrap_or_else(|| "anonymous-guest".to_string());
    if !with_id {
        return name;
    }
    let mut buf = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut buf);
    format!(
        "{name}-{:02x}{:02x}{:02x}{:02x}",
        buf[0], buf[1], buf[2], buf[3]
    )
}

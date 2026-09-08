//! The seam between the pure pipeline logic and the WIT bindings.
//!
//! Everything in this package except `guest.rs` is written against the
//! [`Host`] trait rather than against `sol:actions/action-exec` directly.
//! That is what lets `cargo test` run on the host target: `guest.rs` is
//! `#[cfg(target_arch = "wasm32")]`, so the wit-bindgen surface — which emits
//! `#[link(wasm_import_module = ...)]` externs that cannot resolve in a host
//! test binary — is simply absent when testing.

use serde_json::Value;

/// Everything the guest can reach outside itself.
pub trait Host {
    /// Mirror of `sol:actions/action-exec.exec`, with `output` pre-parsed.
    ///
    /// `Err` means the *host* rejected the call — an internal action that
    /// returned `Err` surfaces here, not as `HostCall { success: false }`.
    /// `success: false` only ever comes back from a wasm or script callee.
    /// Callers must handle both.
    fn exec(&self, action_ref: &str, payload: &Value) -> Result<HostCall, String>;

    fn log(&self, msg: &str);
}

/// A resolved `action-result` from a nested `exec`.
pub struct HostCall {
    pub success: bool,
    pub message: Option<String>,
    /// `Value::Null` when the callee returned no output, or output that did
    /// not parse as JSON.
    pub result: Value,
}

/// Bindgen-free mirror of the WIT `action-result` record.
///
/// `output` is a `Value` rather than a `String`; `guest.rs` stringifies it on
/// the way out, which is what the host then re-parses into the action result.
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
    /// We always return `Ok(ActionResult { success: false, .. })` rather than
    /// `Err(String)`: the host maps `Err` to the same `success: false` but
    /// drops `output` to `Value::Null`, so `Err` is strictly less expressive.
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

/// Read a non-empty string param, trimmed. Absent, null, non-string, and
/// whitespace-only all collapse to `None`.
pub fn str_param(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Truncate on a char boundary, appending an elision marker.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &s[..end], s.len())
}

/// Split a joined `/path/name` reference (e.g.
/// `/packages/solx-ollama/ollama-chat`, or a `paramTypeRef` like
/// `/packages/solx-ollama/ChatParams`) into its path and name — several
/// built-ins (`action_start`, `entity_get_type`, ...) take that pair
/// separately rather than the single joined reference `exec` takes.
pub fn split_ref(reference: &str) -> Option<(&str, &str)> {
    let idx = reference.rfind('/')?;
    let (path, rest) = reference.split_at(idx);
    let name = &rest[1..];
    if path.is_empty() || name.is_empty() {
        None
    } else {
        Some((path, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_two_segment_ref() {
        assert_eq!(split_ref("/packages/solx-ollama/ollama-chat"), Some(("/packages/solx-ollama", "ollama-chat")));
    }

    #[test]
    fn rejects_a_ref_with_no_slash_or_empty_segments() {
        assert_eq!(split_ref("no-slash-here"), None);
        assert_eq!(split_ref("/trailing-slash/"), None);
        assert_eq!(split_ref("/"), None);
    }
}

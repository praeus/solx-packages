//! Shared param plumbing: the connection-carrier struct both actions use, and
//! the search-scope enum both search against.
//!
//! Each action parses its own request shape into a [`Params`] independently -
//! see [`inquire`] for `inquire`'s own parsing and [`multi::parse`] for
//! `multi_inquire`'s, which builds one purely to carry the connection
//! overrides below so [`apply_llm_overrides`]/[`crate::llm::call`] work
//! unchanged for both actions.

pub mod inquire;
pub mod multi;

use serde_json::Value;

/// Ceiling on the per-LLM-call `timeout_secs` a caller may request.
pub const MAX_LLM_TIMEOUT: u64 = 900;

pub const DEFAULT_LLM_ACTION_REF: &str = "/packages/solx-ollama/ollama-chat";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Documents,
    Actions,
    Both,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Documents => "documents",
            Scope::Actions => "actions",
            Scope::Both => "both",
        }
    }

    pub fn searches_documents(&self) -> bool {
        matches!(self, Scope::Documents | Scope::Both)
    }

    pub fn searches_actions(&self) -> bool {
        matches!(self, Scope::Actions | Scope::Both)
    }
}

#[derive(Debug)]
pub struct Params {
    pub inquiry: String,
    pub model: String,
    pub scope: Scope,
    pub max_terms: usize,
    pub max_results: usize,
    pub path_prefix: Option<String>,
    pub type_ref: Option<String>,
    pub inquiry_prompt: Option<String>,
    pub summary_prompt: Option<String>,
    /// Action reference for the chat-completion action this pipeline drives
    /// for both the search-term and summary phases. Defaults to
    /// `solx-ollama`'s `ollama-chat`, but any action taking the same
    /// `{model, messages, format?}` shape and returning Ollama-style
    /// `{message: {content}}` can be swapped in.
    pub llm_action_ref: String,
    /// Passed through verbatim to the llm action call, when present.
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub auth_secret_name: Option<String>,
    pub headers: Option<Value>,
    /// Per-LLM-call timeout, forwarded to each of the two chat calls.
    pub timeout_secs: Option<u64>,
    /// Merged into every call's own `options` object (every payload this
    /// package builds sets `{"temperature": 0}`), a key at a time - a key the
    /// caller supplies overrides this pipeline's own default for it, and every
    /// other key from either side survives untouched. This is the only way to
    /// raise `num_ctx`: an Ollama model's context window is not otherwise
    /// under this pipeline's control, and a prompt that overflows it is
    /// truncated by Ollama - typically from the front, which is where the
    /// grounding rules in every system prompt here live. See
    /// [`crate::host::join_within_budget`] for the other half of that
    /// problem: bounding what this pipeline puts in the prompt in the first
    /// place, which `num_ctx` alone cannot do since it is a ceiling, not a
    /// guarantee that the caller picked one large enough.
    pub options: Option<Value>,
}

/// Merge the connection-related overrides (`base_url`/`api_key`/
/// `auth_secret_name`/`headers`/`timeout_secs`/`options`) into a chat-call
/// payload that already carries `model`/`messages`/etc. Only present fields
/// are added, so the callee's own defaults (env lookup, stored secret, ...)
/// still apply when a caller didn't override anything.
pub fn apply_llm_overrides(mut payload: Value, p: &Params) -> Value {
    let obj = payload.as_object_mut().expect("payload must be an object");
    if let Some(v) = &p.base_url {
        obj.insert("base_url".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &p.api_key {
        obj.insert("api_key".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &p.auth_secret_name {
        obj.insert("auth_secret_name".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &p.headers {
        obj.insert("headers".to_string(), v.clone());
    }
    if let Some(v) = p.timeout_secs {
        obj.insert("timeout_secs".to_string(), Value::from(v));
    }
    // A key at a time, not a wholesale replacement: every payload here already
    // sets its own `options` (at minimum `{"temperature": 0}`), and a caller
    // adding `num_ctx` must not silently lose that default for every key they
    // did not mention. `p.options` wins per key on a collision - a caller who
    // explicitly sets `temperature` gets to override this pipeline's own
    // choice of 0, not just add to it.
    if let Some(overrides) = p.options.as_ref().and_then(Value::as_object) {
        let existing = obj.entry("options".to_string()).or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(existing) = existing.as_object_mut() {
            for (k, v) in overrides {
                existing.insert(k.clone(), v.clone());
            }
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params_with_options(options: Option<Value>) -> Params {
        let mut p = inquire::parse(&json!({ "inquiry": "q", "model": "m" })).unwrap();
        p.options = options;
        p
    }

    #[test]
    fn no_caller_options_leaves_the_payloads_own_options_untouched() {
        let payload = json!({ "model": "m", "options": { "temperature": 0 } });
        let merged = apply_llm_overrides(payload, &params_with_options(None));
        assert_eq!(merged["options"], json!({ "temperature": 0 }));
    }

    #[test]
    fn caller_options_add_a_key_without_disturbing_the_default() {
        // num_ctx is the whole point of this: it is the only way to raise a
        // model's context window from this pipeline, and it must not cost the
        // caller the temperature: 0 every payload already sets.
        let payload = json!({ "model": "m", "options": { "temperature": 0 } });
        let params = params_with_options(Some(json!({ "num_ctx": 8192 })));
        let merged = apply_llm_overrides(payload, &params);
        assert_eq!(merged["options"], json!({ "temperature": 0, "num_ctx": 8192 }));
    }

    #[test]
    fn a_caller_option_overrides_this_pipelines_own_default_for_that_key() {
        let payload = json!({ "model": "m", "options": { "temperature": 0 } });
        let params = params_with_options(Some(json!({ "temperature": 0.7 })));
        let merged = apply_llm_overrides(payload, &params);
        assert_eq!(merged["options"], json!({ "temperature": 0.7 }));
    }

    #[test]
    fn options_are_merged_in_even_if_the_payload_had_none_at_all() {
        let payload = json!({ "model": "m" });
        let params = params_with_options(Some(json!({ "num_ctx": 4096 })));
        let merged = apply_llm_overrides(payload, &params);
        assert_eq!(merged["options"], json!({ "num_ctx": 4096 }));
    }
}

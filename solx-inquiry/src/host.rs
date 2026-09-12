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
#[derive(Debug)]
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

/// Alias every top-level key of `params` under its opposite-case-convention
/// spelling, so a caller need not know that `inquire`'s and `instruct`'s own
/// params are snake_case even though the wider solx ecosystem mixes that with
/// camelCase elsewhere (entity/search params on the solx-core side).
///
/// Duplicated from `solx_actions::internal::normalize_params` rather than
/// shared: this crate compiles to a standalone wasm component with no
/// dependency on solx-core.
///
/// Rules, in order of importance:
/// * An explicit value under either spelling always wins over a *derived*
///   alias — this only ever inserts a key that is entirely absent, so it can
///   never clobber a caller-supplied value, and two independently meaningful
///   keys that already coexist are untouched.
/// * Shallow only: nested values (`headers`, `options`) are opaque payloads,
///   not wire parameter names, and are never rewritten or descended into.
/// * Idempotent.
pub fn normalize_params(params: &Value) -> Value {
    let Some(obj) = params.as_object() else {
        return params.clone();
    };
    let mut out = obj.clone();
    for (key, value) in obj.iter() {
        for alias in [to_camel_case(key), to_snake_case(key)] {
            if alias != *key {
                out.entry(alias).or_insert_with(|| value.clone());
            }
        }
    }
    Value::Object(out)
}

fn to_camel_case(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut upper_next = false;
    for ch in key.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn to_snake_case(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for (i, ch) in key.char_indices() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
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

/// From `lines`, in the order given — highest priority first — a prefix: the
/// first line unconditionally, then as many more as fit within `budget` total
/// characters (including the separating newlines) before the next one would
/// not, at which point every remaining line is dropped.
///
/// This package builds several prompt sections from a ranked or dated list
/// (search hits sorted best-first, memories sorted most-recent-first, session
/// turns) whose own per-item caps (`MAX_DETAILS_CHARS`, `MEMORY_TEXT_CAP`,
/// `HISTORY_TEXT_CAP`, ...) were each tuned in isolation and were never meant
/// to sum against one number — raise `max_results`, `recall_limit` and
/// `history_limit` all at once and the assembled prompt can run well past
/// what a small local model's context window holds, with no error: Ollama
/// truncates silently, typically from the front of the prompt, which is
/// exactly where every system prompt here puts its grounding rules.
///
/// Kept as a prefix rather than filtered to whichever combination of lines
/// fits, because a caller with an explicit priority order wants a clean
/// cutoff at that order — dropping the tail once the budget runs out — not a
/// best-fit selection that could skip something important for being long and
/// keep something unimportant for being short. The first line survives
/// unconditionally so a single line larger than `budget` still leaves
/// something behind rather than an empty prompt section, which would read as
/// "there is nothing here" when there plainly is.
///
/// A caller whose natural order is lowest-priority-first (an oldest-first
/// session history, in this package) reverses the input before calling and
/// reverses the result back.
pub fn take_within_budget(lines: &[String], budget: usize) -> Vec<&str> {
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for line in lines {
        let sep = if kept.is_empty() { 0 } else { 1 };
        if !kept.is_empty() && used + sep + line.len() > budget {
            break;
        }
        used += sep + line.len();
        kept.push(line.as_str());
    }
    kept
}

/// [`take_within_budget`], joined with `\n` — the common case, for a caller
/// whose input is already in priority order and has no reversal to undo.
pub fn join_within_budget(lines: &[String], budget: usize) -> String {
    take_within_budget(lines, budget).join("\n")
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
    use serde_json::json;

    #[test]
    fn normalize_params_aliases_snake_case_to_camel_case_and_back() {
        let out = normalize_params(&json!({"type_ref": "a"}));
        assert_eq!(out.get("typeRef").and_then(|v| v.as_str()), Some("a"));

        let out = normalize_params(&json!({"maxTerms": 5}));
        assert_eq!(out.get("max_terms").and_then(|v| v.as_u64()), Some(5));
    }

    #[test]
    fn normalize_params_never_overwrites_an_explicit_value() {
        let out = normalize_params(&json!({"type_ref": "a", "typeRef": "b"}));
        assert_eq!(out.get("type_ref").and_then(|v| v.as_str()), Some("a"));
        assert_eq!(out.get("typeRef").and_then(|v| v.as_str()), Some("b"));
    }

    #[test]
    fn normalize_params_is_shallow() {
        let out = normalize_params(&json!({"options": {"num_ctx": 1}}));
        let options = out.get("options").unwrap();
        assert!(options.get("numCtx").is_none());
    }

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

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn everything_survives_when_it_all_fits() {
        let input = lines(&["a", "bb", "ccc"]);
        assert_eq!(join_within_budget(&input, 100), "a\nbb\nccc");
    }

    #[test]
    fn drops_a_clean_tail_once_the_budget_runs_out() {
        // "a\nbb" is 4 chars; adding "\nccc" would make 8, over a budget of 5.
        let input = lines(&["a", "bb", "ccc"]);
        assert_eq!(join_within_budget(&input, 5), "a\nbb");
    }

    #[test]
    fn the_first_line_survives_even_alone_over_budget() {
        // An empty result would read as "nothing here" when something plainly
        // exists - worse than one oversized line.
        let input = lines(&["a very long first line indeed", "b"]);
        assert_eq!(join_within_budget(&input, 3), "a very long first line indeed");
    }

    #[test]
    fn an_empty_input_yields_an_empty_result() {
        assert_eq!(join_within_budget(&[], 100), "");
    }

    #[test]
    fn a_lowest_priority_first_caller_reverses_around_the_call() {
        // Mirrors how `session::history_block` uses this: turns are stored
        // oldest-first, so the caller reverses to newest-first before calling
        // (making the *newest* the unconditional survivor and the *oldest*
        // what gets dropped), then reverses the kept prefix back for display.
        let oldest_first = lines(&["oldest", "middle", "newest"]);
        let mut newest_first = oldest_first.clone();
        newest_first.reverse();
        let mut kept: Vec<String> =
            take_within_budget(&newest_first, 6).into_iter().map(str::to_string).collect();
        kept.reverse();
        assert_eq!(kept, vec!["newest".to_string()]);
    }
}

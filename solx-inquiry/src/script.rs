//! Render a model's structured steps into `.solx` source.
//!
//! The model never writes `.solx`. It returns a list of
//! `{action_ref, params, capture}` steps under a `format` schema, and this
//! module turns those into text. That division is the whole reason the action
//! inquiry is worth doing at all with a small local model: the two ways
//! generated `.solx` goes silently wrong are both eliminated here rather than
//! explained in a prompt and hoped for.
//!
//! * **Quoting.** A `--json` argument is single-quoted, and an *unescaped* `'`
//!   inside it does not error — it ends the argument right there, and the rest
//!   of the line becomes unrelated bare tokens that still parse as a valid but
//!   completely different statement. (This is documented in
//!   `solx-core/solx-scripts/src/lib.rs` as having broken a real package's
//!   `install.solx`.) [`escape_single_quoted`] is the only place that can get
//!   it wrong, and it is tested.
//! * **Invented actions.** A step naming an action that does not exist fails
//!   at run time, long after the caller has been handed a script that looks
//!   plausible. [`render`] drops any step whose `action_ref` was not among the
//!   actions the inquiry's own search actually surfaced, and says so in the
//!   script's notes.
//! * **Capture references.** `.solx` substitutes `$name` *textually*, and by
//!   the captured value's runtime type: a captured string is inserted raw and
//!   unquoted, everything else as JSON (`solx-scripts`' `value_to_arg_string`).
//!   So a reference has to be spelled `"$name"` where the parameter wants a
//!   string and bare `$name` where it wants anything else, and the wrong one
//!   yields a `--json` body that fails to parse at run time. Worse, an
//!   *undefined* `$name` is not an error either — solx leaves it in the text
//!   verbatim and the action receives the literal string `"$name"`.
//!   [`render_params`] picks the spelling from the callee's own parameter
//!   schema, and [`render`] drops a step referencing a capture no earlier step
//!   defines.
//!
//! Only `exec` stages are emitted, which is exactly what a `script`-typed
//! action supports (`solx-actions/src/script.rs` accepts `exec` and `json` and
//! nothing else). There is no `return`: a script evaluates to its last
//! statement, which is why the prompt asks for the answering step last.

use std::collections::BTreeMap;

use serde_json::{json, Value};

/// Cap on a rendered script, so one runaway parameter blob cannot dominate the
/// result the caller has to read.
const MAX_SCRIPT_CHARS: usize = 8000;

#[derive(Debug, Clone)]
pub struct Step {
    pub action_ref: String,
    pub params: Value,
    pub capture: Option<String>,
}

/// What one inquiry's search actually surfaced — the ground truth a proposed
/// step is checked and rendered against.
///
/// Bundled rather than passed as three parallel slices because they are three
/// views of one thing (the actions this inquiry may legitimately call) and are
/// always built together, by [`crate::inquiry::prepare`].
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    /// Every action reference the search returned. A step naming anything else
    /// is dropped: it may not exist at all.
    pub allowed: Vec<String>,
    /// Of those, the ones solx would stop for a human decision.
    pub destructive: Vec<String>,
    /// Each action's parameter JSON Schema, keyed by reference, as already
    /// fetched onto the hit by [`crate::search::enrich_hits`]. Absent for an
    /// action whose row declared no `paramTypeRef`, or whose type could not be
    /// read — [`render_params`] falls back to quoting and says so.
    pub param_schemas: BTreeMap<String, Value>,
}

impl Catalogue {
    pub fn allows(&self, action_ref: &str) -> bool {
        self.allowed.iter().any(|r| r == action_ref)
    }

    pub fn is_destructive(&self, action_ref: &str) -> bool {
        self.destructive.iter().any(|r| r == action_ref)
    }

    pub fn param_schema(&self, action_ref: &str) -> Option<&Value> {
        self.param_schemas.get(action_ref)
    }
}

#[derive(Debug, Clone)]
pub struct Script {
    pub title: Option<String>,
    pub notes: Vec<String>,
    pub steps: Vec<Step>,
    pub source: String,
    /// References among `steps` that carry `solx:destructive`. Non-empty means
    /// running this script will stop for a human decision - and, more to the
    /// point, that it does something worth stopping for.
    pub destructive: Vec<String>,
}

impl Script {
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "notes": self.notes,
            "actions": self.steps.iter().map(|s| Value::String(s.action_ref.clone())).collect::<Vec<_>>(),
            "destructive": self.destructive,
            "steps": self.steps.iter().map(|s| json!({
                "action_ref": s.action_ref,
                "params": s.params,
                "capture": s.capture,
            })).collect::<Vec<_>>(),
            "source": self.source,
        })
    }
}

/// Build a script from one model-proposed entry.
///
/// `catalogue` is what the inquiry's search surfaced — which actions may be
/// called, which of them are destructive, and what shape each one's parameters
/// are. `None` when nothing survived: a script with no runnable steps is not a
/// degraded script, it is not a script, and returning one would invite a
/// caller to run an empty file. The reason is not lost — it is reported by
/// [`render`]'s caller from the returned notes of the scripts that did
/// survive, and by the inquiry's own `notes`.
pub fn render(
    title: Option<String>,
    model_notes: Option<String>,
    proposed: &[Step],
    catalogue: &Catalogue,
) -> Option<Script> {
    let mut notes: Vec<String> = model_notes.into_iter().collect();
    let mut steps: Vec<Step> = Vec::new();
    // Capture names an earlier *surviving* step defines, in order. A step is
    // checked against this before its own capture joins it, so a step cannot
    // reference itself, and a dropped step never leaves a definition behind.
    let mut defined: Vec<String> = Vec::new();

    for step in proposed {
        if !catalogue.allows(&step.action_ref) {
            notes.push(format!(
                "dropped a step calling {}: that action was not among the search results, so it may not exist",
                step.action_ref
            ));
            continue;
        }
        if !step.params.is_object() {
            notes.push(format!(
                "dropped a step calling {}: its params were not an object",
                step.action_ref
            ));
            continue;
        }
        // An undefined `$name` is not a run-time error - solx leaves it in the
        // text verbatim and the action receives the literal string "$name". So
        // a reference to a capture nothing defines is dropped here for exactly
        // the reason an invented `action_ref` is: it fails silently and late.
        let undefined = undefined_references(&step.params, &defined);
        if !undefined.is_empty() {
            notes.push(format!(
                "dropped a step calling {}: it references {}, which no earlier step captures",
                step.action_ref,
                undefined.join(", ")
            ));
            continue;
        }
        if let Some(capture) = capture_name(step) {
            defined.push(capture.to_string());
        }
        steps.push(step.clone());
    }

    if steps.is_empty() {
        return None;
    }

    // Rendered up front so the "rendered on an assumption" notes below belong
    // to the steps that actually survive the size cap.
    let statements: Vec<(String, Vec<String>)> =
        steps.iter().map(|step| render_statement(step, catalogue)).collect();

    // The cap drops whole steps rather than truncating the text, because a
    // `.solx` statement cut in half does not fail to parse - it silently
    // becomes a different, valid statement. `steps` is trimmed to match, so
    // what a caller reads and what they would run stay the same thing.
    let mut source = String::new();
    let mut assumed: Vec<String> = Vec::new();
    let mut kept = 0;
    for (statement, unresolved) in &statements {
        if source.len() + statement.len() > MAX_SCRIPT_CHARS {
            notes.push(format!(
                "dropped {} trailing step(s): the rendered script would have exceeded {MAX_SCRIPT_CHARS} characters",
                steps.len() - kept
            ));
            break;
        }
        source.push_str(statement);
        for reference in unresolved {
            if !assumed.contains(reference) {
                assumed.push(reference.clone());
            }
        }
        kept += 1;
    }
    if kept == 0 {
        return None;
    }
    steps.truncate(kept);

    if !assumed.is_empty() {
        notes.push(format!(
            "left {} quoted as a JSON string: the action it is passed to declares no parameter \
             schema, so whether solx substitutes the captured value quoted or bare could not be \
             decided here - check it before running",
            assumed.join(", ")
        ));
    }

    // Surfaced rather than refused. Deleting something can be exactly what was
    // asked for, and this pipeline does not run anything - but a caller told
    // to run these scripts must not have to read the .solx to find out one of
    // them deletes an action.
    let destructive: Vec<String> = steps
        .iter()
        .map(|s| s.action_ref.clone())
        .filter(|r| catalogue.is_destructive(r))
        .fold(Vec::new(), |mut acc, r| {
            if !acc.contains(&r) {
                acc.push(r);
            }
            acc
        });
    if !destructive.is_empty() {
        notes.push(format!(
            "this script calls {}, which solx marks destructive and will stop for a human decision",
            destructive.join(", ")
        ));
    }

    Some(Script { title, notes, steps, source, destructive })
}

/// One `.solx` statement, plus any capture references it had to render on an
/// assumption because the callee declared no parameter schema.
fn render_statement(step: &Step, catalogue: &Catalogue) -> (String, Vec<String>) {
    let mut out = String::new();
    // A capture name is substituted textually as `$name`, so it has to be a
    // bare identifier; anything else is dropped rather than emitted, since a
    // malformed one would corrupt every later statement that reads it.
    if let Some(capture) = capture_name(step) {
        out.push_str(&format!("${capture} = "));
    }
    let mut assumed = Vec::new();
    let params = render_params(&step.params, catalogue.param_schema(&step.action_ref), &mut assumed);
    out.push_str(&format!(
        "exec {} --json '{}';\n",
        step.action_ref,
        escape_single_quoted(&params)
    ));
    (out, assumed)
}

/// The capture this step actually defines, if any: `None` for an absent name
/// and for a malformed one, which [`render_statement`] drops rather than
/// emitting.
fn capture_name(step: &Step) -> Option<&str> {
    step.capture.as_deref().map(str::trim).filter(|c| is_identifier(c))
}

/// A `$name` capture is only legal as a bare identifier.
fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The capture a string references, when the string is *exactly* that
/// reference (`$hits`, `$doc.path`, `$page.items.0`) and nothing else.
///
/// Only whole-value references count. solx would substitute a `$name` buried
/// inside longer text too, but a parameter that is prose containing a `$` is
/// far more likely than one deliberately splicing a capture into the middle of
/// a sentence — and treating the former as a reference would drop a step that
/// works today. A field path segment may be all digits, because
/// `navigate_json_path` indexes arrays that way.
fn capture_reference(s: &str) -> Option<&str> {
    let mut parts = s.strip_prefix('$')?.split('.');
    let base = parts.next()?;
    if !is_identifier(base) {
        return None;
    }
    for part in parts {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
    }
    Some(base)
}

/// Every capture `params` references that `defined` does not contain, spelled
/// as it appears and deduplicated. Ordered by the walk, which for an object is
/// `serde_json::Map` order (by key) rather than the order the model wrote them
/// - deterministic, which is all a note needs.
fn undefined_references(params: &Value, defined: &[String]) -> Vec<String> {
    fn walk(value: &Value, defined: &[String], out: &mut Vec<String>) {
        match value {
            Value::String(s) => {
                if let Some(base) = capture_reference(s) {
                    if !defined.iter().any(|d| d == base) && !out.iter().any(|r| r == s) {
                        out.push(s.clone());
                    }
                }
            }
            Value::Object(map) => map.values().for_each(|v| walk(v, defined, out)),
            Value::Array(items) => items.iter().for_each(|v| walk(v, defined, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(params, defined, &mut out);
    out
}

/// Render params into the text that goes inside `--json '...'`.
///
/// Not `Value::to_string()`, because `.solx` substitution is typed: a captured
/// string is inserted raw and unquoted, everything else as JSON. So a
/// reference must be spelled `"$name"` where the parameter wants a string and
/// bare `$name` where it wants an object, array or number — and the wrong
/// spelling produces a `--json` body that fails to parse at run time, long
/// after the script looked plausible.
///
/// The callee's own `paramSchema` decides which, walking down alongside the
/// value so a nested parameter is judged by its own declared type. With no
/// schema to consult the reference stays quoted — right for the scalar case a
/// model writes most often — and is reported back so the caller is told what
/// was assumed rather than left to find out by running it.
pub fn render_params(params: &Value, schema: Option<&Value>, assumed: &mut Vec<String>) -> String {
    match params {
        Value::String(s) => match capture_reference(s) {
            None => Value::String(s.clone()).to_string(),
            Some(_) => match declares_string(schema) {
                // Declared a string: solx inserts the captured text raw, so
                // the quotes around it are what make the result valid JSON.
                Some(true) => Value::String(s.clone()).to_string(),
                // Declared as something else: the captured value serializes to
                // JSON on its own and quoting it would nest JSON inside a
                // string.
                Some(false) => s.clone(),
                None => {
                    if !assumed.iter().any(|r| r == s) {
                        assumed.push(s.clone());
                    }
                    Value::String(s.clone()).to_string()
                }
            },
        },
        Value::Object(map) => {
            let properties = schema.and_then(|s| s.get("properties"));
            let parts: Vec<String> = map
                .iter()
                .map(|(key, value)| {
                    let child = properties.and_then(|p| p.get(key));
                    format!(
                        "{}:{}",
                        Value::String(key.clone()),
                        render_params(value, child, assumed)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let child = schema.and_then(|s| s.get("items"));
            let parts: Vec<String> =
                items.iter().map(|v| render_params(v, child, assumed)).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// `Some(true)` when this schema node declares a string and nothing else,
/// `Some(false)` when it declares something that is not a string, `None` when
/// there is nothing to go on.
///
/// A `["string","null"]` union is still a string as far as substitution is
/// concerned — an optional string parameter is passed as a quoted string or
/// omitted, never as bare JSON. `null` is therefore ignored rather than
/// making the union undecidable.
fn declares_string(schema: Option<&Value>) -> Option<bool> {
    match schema?.get("type")? {
        Value::String(name) => Some(name == "string"),
        Value::Array(names) => {
            let declared: Vec<&str> = names
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| *name != "null")
                .collect();
            if declared.is_empty() {
                None
            } else {
                Some(declared.iter().all(|name| *name == "string"))
            }
        }
        _ => None,
    }
}

/// Escape a string for the inside of a `.solx` single-quoted argument.
///
/// Only `'` matters. The `.solx` tokenizer honors a backslash before a single
/// quote inside a single-quoted argument and passes the quote through; JSON
/// itself never needs `'` escaped, so what reaches `serde_json` is valid
/// either way. A backslash is *not* escaped, because the escape is understood
/// only in front of a quote — doubling backslashes here would corrupt every
/// Windows path and every regex in a parameter value.
pub fn escape_single_quoted(s: &str) -> String {
    s.replace('\'', "\\'")
}

/// Pull the steps out of one model-proposed script object.
pub fn parse_steps(value: &Value) -> Vec<Step> {
    value
        .get("steps")
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter_map(|s| {
                    let action_ref = s.get("action_ref").and_then(Value::as_str)?.trim();
                    if action_ref.is_empty() {
                        return None;
                    }
                    Some(Step {
                        action_ref: action_ref.to_string(),
                        params: s.get("params").cloned().unwrap_or_else(|| json!({})),
                        capture: s
                            .get("capture")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|c| !c.is_empty())
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn step(action_ref: &str, params: Value, capture: Option<&str>) -> Step {
        Step {
            action_ref: action_ref.to_string(),
            params,
            capture: capture.map(str::to_string),
        }
    }

    /// A catalogue that allows every named reference and knows nothing about
    /// their parameter shapes — the common case in these tests.
    fn allowing(refs: &[&str]) -> Catalogue {
        Catalogue {
            allowed: refs.iter().map(|r| r.to_string()).collect(),
            ..Catalogue::default()
        }
    }

    fn with_schema(action_ref: &str, schema: Value) -> Catalogue {
        let mut catalogue = allowing(&[action_ref]);
        catalogue.param_schemas.insert(action_ref.to_string(), schema);
        catalogue
    }

    #[test]
    fn renders_one_exec_statement_per_step() {
        let steps = vec![
            step("/builtin/document/search_documents", json!({ "q": "auth" }), Some("hits")),
            step("/builtin/console/print", json!({ "message": "done" }), None),
        ];
        let catalogue =
            allowing(&["/builtin/document/search_documents", "/builtin/console/print"]);
        let script = render(None, None, &steps, &catalogue).unwrap();
        assert_eq!(
            script.source,
            "$hits = exec /builtin/document/search_documents --json '{\"q\":\"auth\"}';\n\
             exec /builtin/console/print --json '{\"message\":\"done\"}';\n"
        );
    }

    #[test]
    fn an_apostrophe_in_a_parameter_is_escaped() {
        // Unescaped, this would end the --json argument early and turn the
        // rest of the line into unrelated bare tokens - silently, with no
        // parse error at all.
        let steps = vec![step("/x/y", json!({ "note": "don't break" }), None)];
        let script = render(None, None, &steps, &allowing(&["/x/y"])).unwrap();
        assert!(script.source.contains("don\\'t"), "{}", script.source);
    }

    #[test]
    fn a_backslash_is_left_alone() {
        // The escape is only understood in front of a quote, so doubling
        // backslashes would corrupt every Windows path in a parameter.
        assert_eq!(escape_single_quoted(r"C:\tmp\x"), r"C:\tmp\x");
        assert_eq!(escape_single_quoted("it's"), "it\\'s");
    }

    #[test]
    fn a_step_naming_an_unsurfaced_action_is_dropped_with_a_note() {
        let steps = vec![
            step("/builtin/document/search_documents", json!({}), None),
            step("/invented/action", json!({}), None),
        ];
        let catalogue = allowing(&["/builtin/document/search_documents"]);
        let script = render(None, None, &steps, &catalogue).unwrap();
        assert_eq!(script.steps.len(), 1);
        assert_eq!(script.notes.len(), 1);
        assert!(script.notes[0].contains("/invented/action"), "{:?}", script.notes);
    }

    #[test]
    fn a_script_left_with_no_steps_is_discarded() {
        let steps = vec![step("/invented/action", json!({}), None)];
        assert!(render(None, None, &steps, &allowing(&["/real/action"])).is_none());
        assert!(render(None, None, &[], &Catalogue::default()).is_none());
    }

    #[test]
    fn a_malformed_capture_name_is_dropped_rather_than_emitted() {
        let steps = vec![step("/x/y", json!({}), Some("not a name"))];
        let script = render(None, None, &steps, &allowing(&["/x/y"])).unwrap();
        assert!(script.source.starts_with("exec /x/y"), "{}", script.source);
    }

    // ── capture references ──────────────────────────────────────────────────

    #[test]
    fn a_capture_reference_is_left_bare_when_the_parameter_is_not_a_string() {
        // solx substitutes an object capture as JSON, so quoting it would nest
        // JSON inside a string and the --json body would fail to parse.
        let steps = vec![
            step("/x/find", json!({ "q": "auth" }), Some("hits")),
            step("/x/save", json!({ "document": "$hits" }), None),
        ];
        let mut catalogue = allowing(&["/x/find", "/x/save"]);
        catalogue.param_schemas.insert(
            "/x/save".to_string(),
            json!({ "type": "object", "properties": { "document": { "type": "object" } } }),
        );
        let script = render(None, None, &steps, &catalogue).unwrap();
        assert!(script.source.contains(r#"{"document":$hits}"#), "{}", script.source);
        assert!(script.notes.is_empty(), "{:?}", script.notes);
    }

    #[test]
    fn a_capture_reference_stays_quoted_when_the_parameter_is_a_string() {
        // A captured string substitutes raw, so the quotes are what keep the
        // rendered JSON valid.
        let steps = vec![
            step("/x/find", json!({ "q": "auth" }), Some("doc")),
            step("/x/read", json!({ "path": "$doc.path" }), None),
        ];
        let mut catalogue = allowing(&["/x/find", "/x/read"]);
        catalogue.param_schemas.insert(
            "/x/read".to_string(),
            json!({ "type": "object", "properties": { "path": { "type": ["string", "null"] } } }),
        );
        let script = render(None, None, &steps, &catalogue).unwrap();
        assert!(script.source.contains(r#"{"path":"$doc.path"}"#), "{}", script.source);
        assert!(script.notes.is_empty(), "{:?}", script.notes);
    }

    #[test]
    fn an_undecidable_reference_stays_quoted_and_says_so() {
        // No schema to consult: quoting is right for the scalar case a model
        // writes most often, but the caller is told what was assumed rather
        // than left to discover it by running the script.
        let steps = vec![
            step("/x/find", json!({}), Some("hits")),
            step("/x/use", json!({ "value": "$hits.items" }), None),
        ];
        let script = render(None, None, &steps, &allowing(&["/x/find", "/x/use"])).unwrap();
        assert!(script.source.contains(r#"{"value":"$hits.items"}"#), "{}", script.source);
        assert!(
            script.notes.iter().any(|n| n.contains("$hits.items") && n.contains("quoted")),
            "{:?}",
            script.notes
        );
    }

    #[test]
    fn a_step_referencing_a_capture_nothing_defines_is_dropped() {
        // solx leaves an undefined $name in the text verbatim, so the action
        // would receive the literal string "$hitz" and fail late, or worse,
        // succeed against the wrong input.
        let steps = vec![
            step("/x/find", json!({}), Some("hits")),
            step("/x/use", json!({ "value": "$hitz" }), None),
        ];
        let script = render(None, None, &steps, &allowing(&["/x/find", "/x/use"])).unwrap();
        assert_eq!(script.steps.len(), 1);
        assert!(
            script.notes.iter().any(|n| n.contains("$hitz") && n.contains("no earlier step")),
            "{:?}",
            script.notes
        );
    }

    #[test]
    fn a_step_cannot_reference_its_own_capture_or_a_later_one() {
        let steps = vec![
            step("/x/use", json!({ "value": "$self" }), Some("self")),
            step("/x/find", json!({}), Some("later")),
        ];
        let script = render(None, None, &steps, &allowing(&["/x/find", "/x/use"])).unwrap();
        assert_eq!(script.steps.len(), 1);
        assert_eq!(script.steps[0].action_ref, "/x/find");
    }

    #[test]
    fn a_dropped_step_leaves_no_capture_behind_for_a_later_one_to_use() {
        // The defining step named an action that was never surfaced, so its
        // capture does not exist at run time either.
        let steps = vec![
            step("/invented/find", json!({}), Some("hits")),
            step("/x/use", json!({ "value": "$hits" }), None),
        ];
        assert!(render(None, None, &steps, &allowing(&["/x/use"])).is_none());
    }

    #[test]
    fn a_malformed_capture_name_defines_nothing() {
        // The name is not emitted, so nothing can read it back.
        let steps = vec![
            step("/x/find", json!({}), Some("not a name")),
            step("/x/use", json!({ "value": "$not" }), None),
        ];
        let script = render(None, None, &steps, &allowing(&["/x/find", "/x/use"])).unwrap();
        assert_eq!(script.steps.len(), 1);
    }

    #[test]
    fn only_a_whole_value_reference_counts_as_one() {
        // A `$` inside prose is ordinary text - solx would substitute a known
        // name there, but treating it as a reference would drop steps that
        // work. `$5` is not an identifier and names nothing.
        assert_eq!(capture_reference("$hits"), Some("hits"));
        assert_eq!(capture_reference("$doc.path"), Some("doc"));
        assert_eq!(capture_reference("$page.items.0"), Some("page"));
        assert_eq!(capture_reference("costs $5.00"), None);
        assert_eq!(capture_reference("$5"), None);
        assert_eq!(capture_reference("see $hits for details"), None);
        assert_eq!(capture_reference("$"), None);
        assert_eq!(capture_reference("$doc."), None);
        assert_eq!(capture_reference("plain"), None);
    }

    #[test]
    fn references_are_found_at_any_depth_in_the_params() {
        let params = json!({ "outer": { "inner": ["$a", { "deep": "$b" }] }, "flat": "$c" });
        let undefined = undefined_references(&params, &["b".to_string()]);
        // Object keys walk in map order, so "flat" comes before "outer"; `$b`
        // is defined and so is absent either way.
        assert_eq!(undefined, vec!["$c", "$a"]);
    }

    #[test]
    fn a_nested_parameter_is_judged_by_its_own_declared_type() {
        let steps = vec![
            step("/x/find", json!({}), Some("hits")),
            step("/x/y", json!({ "outer": { "body": "$hits", "label": "$hits" } }), None),
        ];
        let mut catalogue = allowing(&["/x/find", "/x/y"]);
        catalogue.param_schemas.insert(
            "/x/y".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "outer": {
                        "type": "object",
                        "properties": {
                            "body": { "type": "object" },
                            "label": { "type": "string" },
                        }
                    }
                }
            }),
        );
        let script = render(None, None, &steps, &catalogue).unwrap();
        assert!(script.source.contains(r#""body":$hits"#), "{}", script.source);
        assert!(script.source.contains(r#""label":"$hits""#), "{}", script.source);
    }

    #[test]
    fn ordinary_params_render_exactly_as_json_would() {
        // Everything that is not a capture reference must survive untouched,
        // including the characters JSON escapes.
        let params = json!({
            "s": "a \"quoted\" \\ back\nslash",
            "n": 4.5,
            "b": true,
            "z": null,
            "a": [1, "two", { "three": false }],
        });
        let mut assumed = Vec::new();
        let rendered = render_params(&params, None, &mut assumed);
        assert!(assumed.is_empty());
        assert_eq!(serde_json::from_str::<Value>(&rendered).expect("valid JSON"), params);
    }

    #[test]
    fn declares_string_reads_unions_and_gives_up_on_nothing_to_go_on() {
        assert_eq!(declares_string(Some(&json!({ "type": "string" }))), Some(true));
        assert_eq!(declares_string(Some(&json!({ "type": ["string", "null"] }))), Some(true));
        assert_eq!(declares_string(Some(&json!({ "type": "object" }))), Some(false));
        assert_eq!(declares_string(Some(&json!({ "type": ["integer", "null"] }))), Some(false));
        assert_eq!(declares_string(Some(&json!({ "type": ["null"] }))), None);
        assert_eq!(declares_string(Some(&json!({ "description": "x" }))), None);
        assert_eq!(declares_string(None), None);
    }

    // ── size cap and destructive marking ────────────────────────────────────

    #[test]
    fn the_size_cap_drops_whole_steps_rather_than_cutting_one_in_half() {
        // A .solx statement cut mid-argument does not fail to parse - it
        // silently becomes a different, valid statement, which is the one
        // failure mode this module exists to make impossible.
        let big = json!({ "blob": "x".repeat(MAX_SCRIPT_CHARS) });
        let steps = vec![
            step("/x/y", json!({ "q": "small" }), None),
            step("/x/y", big, None),
            step("/x/y", json!({ "q": "also small" }), None),
        ];
        let script = render(None, None, &steps, &allowing(&["/x/y"])).unwrap();

        assert_eq!(script.steps.len(), 1, "only the first step fits");
        assert!(script.source.len() <= MAX_SCRIPT_CHARS);
        // Every emitted statement is whole.
        assert!(script.source.ends_with(";\n"), "{}", script.source);
        assert_eq!(script.source.matches("exec ").count(), 1);
        // `steps` and `source` describe the same script, so a caller reading
        // one and running the other cannot disagree.
        assert_eq!(script.source.matches("exec ").count(), script.steps.len());
        assert!(
            script.notes.iter().any(|n| n.contains("dropped 2 trailing step(s)")),
            "{:?}",
            script.notes
        );
    }

    #[test]
    fn an_assumption_note_belongs_to_a_step_that_survived_the_cap() {
        // The reference is in a trailing step the size cap drops, so warning
        // about it would point at something the caller was never handed.
        let steps = vec![
            step("/x/y", json!({ "blob": "x".repeat(MAX_SCRIPT_CHARS - 50) }), Some("big")),
            step("/x/y", json!({ "value": "$big" }), None),
        ];
        let script = render(None, None, &steps, &allowing(&["/x/y"])).unwrap();
        assert_eq!(script.steps.len(), 1);
        assert!(!script.notes.iter().any(|n| n.contains("$big")), "{:?}", script.notes);
    }

    #[test]
    fn a_first_step_too_big_to_render_yields_no_script() {
        let steps = vec![step("/x/y", json!({ "blob": "x".repeat(MAX_SCRIPT_CHARS) }), None)];
        assert!(render(None, None, &steps, &allowing(&["/x/y"])).is_none());
    }

    #[test]
    fn a_script_calling_a_destructive_action_says_so() {
        // The caller is expected to *run* these scripts, so "this deletes an
        // action" must not be something they only discover by reading the
        // rendered .solx.
        let steps = vec![
            step("/builtin/document/search_documents", json!({ "q": "x" }), None),
            step("/builtin/action/entity_delete_action", json!({ "name": "x" }), None),
        ];
        let mut catalogue = allowing(&[
            "/builtin/document/search_documents",
            "/builtin/action/entity_delete_action",
        ]);
        catalogue.destructive = vec!["/builtin/action/entity_delete_action".to_string()];
        let script = render(None, None, &steps, &catalogue).unwrap();

        assert_eq!(script.destructive, vec!["/builtin/action/entity_delete_action"]);
        assert!(script.notes.iter().any(|n| n.contains("destructive")), "{:?}", script.notes);
        // Surfaced, not refused: deleting something can be exactly what was
        // asked for, and this module does not run anything.
        assert_eq!(script.steps.len(), 2);
    }

    #[test]
    fn a_script_calling_nothing_destructive_carries_no_warning() {
        let steps = vec![step("/x/y", json!({}), None)];
        let mut catalogue = allowing(&["/x/y"]);
        catalogue.destructive = vec!["/other/thing".to_string()];
        let script = render(None, None, &steps, &catalogue).unwrap();
        assert!(script.destructive.is_empty());
        assert!(script.notes.is_empty(), "{:?}", script.notes);
    }

    #[test]
    fn non_object_params_are_refused() {
        let steps = vec![step("/x/y", json!("a string"), None)];
        assert!(render(None, None, &steps, &allowing(&["/x/y"])).is_none());
    }

    #[test]
    fn parses_steps_and_skips_entries_with_no_action_ref() {
        let value = json!({ "steps": [
            { "action_ref": "/x/y", "params": { "a": 1 }, "capture": "r" },
            { "params": { "a": 2 } },
            { "action_ref": "  " },
            { "action_ref": "/x/z" },
        ]});
        let steps = parse_steps(&value);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].capture.as_deref(), Some("r"));
        // An absent params object is an empty one, not a reason to drop the
        // step: plenty of actions take no parameters at all.
        assert_eq!(steps[1].params, json!({}));
    }

    #[test]
    fn a_catalogue_carries_what_a_step_is_checked_against() {
        let catalogue = with_schema("/x/y", json!({ "type": "object" }));
        assert!(catalogue.allows("/x/y"));
        assert!(!catalogue.allows("/x/z"));
        assert!(!catalogue.is_destructive("/x/y"));
        assert!(catalogue.param_schema("/x/y").is_some());
        assert!(catalogue.param_schema("/x/z").is_none());
    }
}

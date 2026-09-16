//! Validate a model's structured steps and hand them back as-is.
//!
//! The model never writes `.solx`, and nothing here does either. It returns a
//! list of `{action_ref, params, capture}` steps under a `format` schema, and
//! this module checks that list against what the inquiry's own search
//! actually surfaced. That is the whole job: a caller executes the returned
//! steps directly, in order, substituting each `capture` reference with the
//! real value an earlier step produced — this module has no opinion on how,
//! since a JSON value substituted into a JSON value has no quoting or typing
//! question the way text spliced into text does.
//!
//! Two failure modes are eliminated here rather than explained in a prompt
//! and hoped for:
//!
//! * **Invented actions.** A step naming an action that does not exist would
//!   fail at run time, long after the caller has been handed a plan that
//!   looks plausible. [`assemble`] drops any step whose `action_ref` was not
//!   among the actions the inquiry's own search actually surfaced, and says
//!   so in the plan's notes.
//! * **Capture references.** A step can reference an earlier step's capture
//!   as `$name` or `$name.field` for one field of it. A reference to a
//!   capture nothing defines has no value to substitute at run time, so
//!   [`assemble`] drops a step referencing a capture no earlier surviving
//!   step defines, the same way it drops a step naming an invented action.

use serde_json::{json, Value};

/// Cap on a returned plan's total serialized size, so one runaway parameter
/// blob cannot dominate the result the caller has to read.
const MAX_STEPS_BYTES: usize = 8000;

#[derive(Debug, Clone)]
pub struct Step {
    pub action_ref: String,
    pub params: Value,
    pub capture: Option<String>,
}

impl Step {
    fn to_json(&self) -> Value {
        json!({
            "action_ref": self.action_ref,
            "params": self.params,
            "capture": self.capture,
        })
    }
}

/// What one inquiry's search actually surfaced — the ground truth a proposed
/// step is checked against.
///
/// Bundled rather than passed as two parallel slices because they are two
/// views of one thing (the actions this inquiry may legitimately call) and
/// are always built together, by [`crate::inquiry::prepare`].
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    /// Every action reference the search returned. A step naming anything else
    /// is dropped: it may not exist at all.
    pub allowed: Vec<String>,
    /// Of those, the ones solx would stop for a human decision.
    pub destructive: Vec<String>,
}

impl Catalogue {
    pub fn allows(&self, action_ref: &str) -> bool {
        self.allowed.iter().any(|r| r == action_ref)
    }

    pub fn is_destructive(&self, action_ref: &str) -> bool {
        self.destructive.iter().any(|r| r == action_ref)
    }
}

#[derive(Debug, Clone)]
pub struct Script {
    pub title: Option<String>,
    pub notes: Vec<String>,
    pub steps: Vec<Step>,
    /// References among `steps` that carry `solx:destructive`. Non-empty means
    /// running this plan will do something worth stopping for.
    pub destructive: Vec<String>,
}

impl Script {
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "notes": self.notes,
            "actions": self.steps.iter().map(|s| Value::String(s.action_ref.clone())).collect::<Vec<_>>(),
            "destructive": self.destructive,
            "steps": self.steps.iter().map(Step::to_json).collect::<Vec<_>>(),
        })
    }
}

/// Validate one model-proposed entry against what the inquiry's search
/// surfaced.
///
/// `catalogue` says which actions may be called and which of them are
/// destructive. `None` when nothing survived: a plan with no runnable steps
/// is not a degraded plan, it is not a plan, and returning one would invite a
/// caller to execute an empty list. The reason is not lost — it is reported
/// by [`assemble`]'s caller from the returned notes of the plans that did
/// survive, and by the inquiry's own `notes`.
pub fn assemble(
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
        // A capture reference with nothing to substitute at run time fails
        // exactly the way an invented `action_ref` does: silently and late.
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

    // The cap drops whole trailing steps rather than any partial one, so a
    // caller executing what it was handed never runs a step that was cut off
    // mid-parameter.
    let mut total = 0usize;
    let mut kept = 0;
    for step in &steps {
        let size = step.to_json().to_string().len();
        if total + size > MAX_STEPS_BYTES {
            notes.push(format!(
                "dropped {} trailing step(s): the returned steps would have exceeded {MAX_STEPS_BYTES} bytes",
                steps.len() - kept
            ));
            break;
        }
        total += size;
        kept += 1;
    }
    if kept == 0 {
        return None;
    }
    steps.truncate(kept);

    // Surfaced rather than refused. Deleting something can be exactly what was
    // asked for, and this pipeline does not run anything - but a caller told
    // to execute these steps must not have to inspect each one to find out
    // one of them deletes an action.
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
            "this plan calls {}, which solx marks destructive and will stop for a human decision",
            destructive.join(", ")
        ));
    }

    Some(Script { title, notes, steps, destructive })
}

/// The capture this step actually defines, if any: `None` for an absent name
/// and for a malformed one, which is dropped rather than kept, since a
/// malformed name is not a legal `$name` reference for a later step to make.
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
/// Only whole-value references count. A caller substituting captures would
/// presumably do the same for a `$name` buried inside longer text, but a
/// parameter that is prose containing a `$` is far more likely than one
/// deliberately splicing a capture into the middle of a sentence — and
/// treating the former as a reference would drop a step that works today. A
/// field path segment may be all digits, to index into an array.
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

    /// A catalogue that allows every named reference.
    fn allowing(refs: &[&str]) -> Catalogue {
        Catalogue { allowed: refs.iter().map(|r| r.to_string()).collect(), ..Catalogue::default() }
    }

    #[test]
    fn assembles_the_steps_that_survive_validation() {
        let steps = vec![
            step("/builtin/document/search_documents", json!({ "q": "auth" }), Some("hits")),
            step("/builtin/console/print", json!({ "message": "done" }), None),
        ];
        let catalogue =
            allowing(&["/builtin/document/search_documents", "/builtin/console/print"]);
        let script = assemble(None, None, &steps, &catalogue).unwrap();
        assert_eq!(script.steps.len(), 2);
        assert_eq!(script.steps[0].action_ref, "/builtin/document/search_documents");
        assert_eq!(script.steps[0].capture.as_deref(), Some("hits"));
        assert_eq!(script.steps[1].params, json!({ "message": "done" }));
    }

    #[test]
    fn a_step_naming_an_unsurfaced_action_is_dropped_with_a_note() {
        let steps = vec![
            step("/builtin/document/search_documents", json!({}), None),
            step("/invented/action", json!({}), None),
        ];
        let catalogue = allowing(&["/builtin/document/search_documents"]);
        let script = assemble(None, None, &steps, &catalogue).unwrap();
        assert_eq!(script.steps.len(), 1);
        assert_eq!(script.notes.len(), 1);
        assert!(script.notes[0].contains("/invented/action"), "{:?}", script.notes);
    }

    #[test]
    fn a_plan_left_with_no_steps_is_discarded() {
        let steps = vec![step("/invented/action", json!({}), None)];
        assert!(assemble(None, None, &steps, &allowing(&["/real/action"])).is_none());
        assert!(assemble(None, None, &[], &Catalogue::default()).is_none());
    }

    #[test]
    fn a_malformed_capture_name_is_kept_on_the_step_but_defines_nothing() {
        let steps = vec![
            step("/x/y", json!({}), Some("not a name")),
            step("/x/use", json!({ "value": "$not" }), None),
        ];
        let script = assemble(None, None, &steps, &allowing(&["/x/y", "/x/use"])).unwrap();
        // The malformed name is not a legal reference, so nothing can read it
        // back - the second step is dropped for referencing an undefined
        // capture.
        assert_eq!(script.steps.len(), 1);
    }

    // ── capture references ──────────────────────────────────────────────────

    #[test]
    fn a_step_referencing_a_capture_nothing_defines_is_dropped() {
        let steps = vec![
            step("/x/find", json!({}), Some("hits")),
            step("/x/use", json!({ "value": "$hitz" }), None),
        ];
        let script = assemble(None, None, &steps, &allowing(&["/x/find", "/x/use"])).unwrap();
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
        let script = assemble(None, None, &steps, &allowing(&["/x/find", "/x/use"])).unwrap();
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
        assert!(assemble(None, None, &steps, &allowing(&["/x/use"])).is_none());
    }

    #[test]
    fn only_a_whole_value_reference_counts_as_one() {
        // A `$` inside prose is ordinary text; `$5` is not an identifier and
        // names nothing.
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

    // ── size cap and destructive marking ────────────────────────────────────

    #[test]
    fn the_size_cap_drops_whole_trailing_steps() {
        let big = json!({ "blob": "x".repeat(MAX_STEPS_BYTES) });
        let steps = vec![
            step("/x/y", json!({ "q": "small" }), None),
            step("/x/y", big, None),
            step("/x/y", json!({ "q": "also small" }), None),
        ];
        let script = assemble(None, None, &steps, &allowing(&["/x/y"])).unwrap();

        assert_eq!(script.steps.len(), 1, "only the first step fits");
        assert!(
            script.notes.iter().any(|n| n.contains("dropped 2 trailing step(s)")),
            "{:?}",
            script.notes
        );
    }

    #[test]
    fn a_first_step_too_big_to_keep_yields_no_plan() {
        let steps = vec![step("/x/y", json!({ "blob": "x".repeat(MAX_STEPS_BYTES) }), None)];
        assert!(assemble(None, None, &steps, &allowing(&["/x/y"])).is_none());
    }

    #[test]
    fn a_plan_calling_a_destructive_action_says_so() {
        // The caller is expected to *execute* these plans, so "this deletes an
        // action" must not be something they only discover by inspecting the
        // steps themselves.
        let steps = vec![
            step("/builtin/document/search_documents", json!({ "q": "x" }), None),
            step("/builtin/action/entity_delete_action", json!({ "name": "x" }), None),
        ];
        let mut catalogue = allowing(&[
            "/builtin/document/search_documents",
            "/builtin/action/entity_delete_action",
        ]);
        catalogue.destructive = vec!["/builtin/action/entity_delete_action".to_string()];
        let script = assemble(None, None, &steps, &catalogue).unwrap();

        assert_eq!(script.destructive, vec!["/builtin/action/entity_delete_action"]);
        assert!(script.notes.iter().any(|n| n.contains("destructive")), "{:?}", script.notes);
        // Surfaced, not refused: deleting something can be exactly what was
        // asked for, and this module does not run anything.
        assert_eq!(script.steps.len(), 2);
    }

    #[test]
    fn a_plan_calling_nothing_destructive_carries_no_warning() {
        let steps = vec![step("/x/y", json!({}), None)];
        let mut catalogue = allowing(&["/x/y"]);
        catalogue.destructive = vec!["/other/thing".to_string()];
        let script = assemble(None, None, &steps, &catalogue).unwrap();
        assert!(script.destructive.is_empty());
        assert!(script.notes.is_empty(), "{:?}", script.notes);
    }

    #[test]
    fn non_object_params_are_refused() {
        let steps = vec![step("/x/y", json!("a string"), None)];
        assert!(assemble(None, None, &steps, &allowing(&["/x/y"])).is_none());
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
        let catalogue = allowing(&["/x/y"]);
        assert!(catalogue.allows("/x/y"));
        assert!(!catalogue.allows("/x/z"));
        assert!(!catalogue.is_destructive("/x/y"));
    }
}

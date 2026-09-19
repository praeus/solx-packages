//! Tagged console output for the `prompt` action.
//!
//! Every milestone of a run is printed to the action's own console with **both**
//! a human-readable tagged message and a machine-readable `data` object, so a
//! caller can reconstruct the run afterwards from `/builtin/console/read`
//! without re-running anything. The point of the tag grammar is that it is
//! parseable: `[prompt:<phase>]` or `[prompt:<phase>:<step>]`, one vocabulary,
//! defined here and nowhere else.
//!
//! On top of that, every milestone carries a **progress event** under the
//! reserved `data.ev` key — see [`ev`]. The tag grammar is the human affordance;
//! the envelope is the wire format. A UI that wants to render progress switches
//! on `ev.t` and never has to regex prose. Two reasons it lives *beside* the
//! existing payload rather than replacing it: the `data` shapes are a declared
//! contract, and prefixes nest — `console/copy` prepends its own `[label]` to an
//! already tagged message, so the first bracketed group in a message is not
//! reliably the tag.
//!
//! The vocabulary here is **flat**. solx-inquiry's indexes every event by which
//! of its parallel inquiries produced it; this pipeline runs one thing at a
//! time, so a phase name places any event on its own and a consumer never has
//! to correlate an index.
//!
//! Printing is best-effort throughout. A console hiccup is an observability
//! problem, never a reason to fail a pipeline that is otherwise producing
//! correct results — the same contract [`crate::llm::drain_console`] keeps. The
//! corollary binds every *consumer*: any single event can be lost, so nothing
//! may require that event A arrived before event B is understood.

use serde_json::{json, Map, Value};

use crate::host::Host;
use crate::llm::CONSOLE_PRINT_REF;

/// Root of every tag this package prints. A caller filtering console entries
/// for a run matches on this prefix.
pub const TAG_ROOT: &str = "prompt";

pub const PHASE_RUN: &str = "run";
pub const PHASE_RECALL: &str = "recall";
pub const PHASE_CONTEXT: &str = "context";
pub const PHASE_INTENT: &str = "intent";
pub const PHASE_SEARCH: &str = "search";
pub const PHASE_STEPS: &str = "steps";
pub const PHASE_RESULT: &str = "result";

pub const STEP_START: &str = "start";
pub const STEP_DONE: &str = "done";
pub const STEP_HITS: &str = "hits";
pub const STEP_RESULT: &str = "result";
pub const STEP_ERROR: &str = "error";
pub const STEP_CANCELLED: &str = "cancelled";

/// `[prompt:<phase>]`.
pub fn phase_tag(phase: &str) -> String {
    format!("{TAG_ROOT}:{phase}")
}

/// `[prompt:<phase>:<step>]`.
///
/// A phase that prints more than once needs it: `intent` reports both its start
/// and its decision, and a consumer matching on the bare `[prompt:intent]`
/// prefix would otherwise pick up whichever came first. One tag, one kind of
/// print, at every level of the grammar.
pub fn phase_step_tag(phase: &str, step: &str) -> String {
    format!("{}:{}", phase_tag(phase), step)
}

/// Major version of the `data.ev` envelope.
///
/// Adding a new event type, or a new *optional* field to an existing one, keeps
/// this at 1. Renaming or removing a field bumps it. A consumer that sees a
/// higher version drops the event rather than guessing at it.
pub const EV_VERSION: u64 = 1;

/// The one key under `data` this package reserves for itself.
///
/// Nested rather than spread across the top of `data` so it can never collide
/// with a domain key — an intent payload already has a `mode`, and an event
/// wants one too.
pub const EV_KEY: &str = "ev";

/// How much of a prompt or a look-up question rides along as `ev.q`.
///
/// Capped because `data` is not a free channel: solx-mcp inlines the whole
/// object into each progress notification's text
/// (`solx-mcp/src/server.rs::send_progress`), so every byte here lands in an
/// MCP client's progress line.
pub const Q_CAP: usize = 160;

pub const EV_RUN_STARTED: &str = "run.started";
pub const EV_RUN_DONE: &str = "run.done";
pub const EV_RUN_FAILED: &str = "run.failed";
pub const EV_RUN_CANCELLED: &str = "run.cancelled";
pub const EV_RECALL_DONE: &str = "recall.done";
pub const EV_CONTEXT_DONE: &str = "context.done";
pub const EV_INTENT_STARTED: &str = "intent.started";
pub const EV_INTENT_DONE: &str = "intent.done";
pub const EV_INTENT_FAILED: &str = "intent.failed";
pub const EV_SEARCH_STARTED: &str = "search.started";
pub const EV_SEARCH_DONE: &str = "search.done";
pub const EV_STEPS_STARTED: &str = "steps.started";
pub const EV_STEPS_DONE: &str = "steps.done";
pub const EV_STEPS_FAILED: &str = "steps.failed";

/// Build one progress event: `{v, t, ...fields}`.
///
/// Field names are short (`q`, `kind`, `ok`, `n`) for the same reason [`Q_CAP`]
/// exists.
pub fn ev(t: &str, fields: Value) -> Value {
    let mut out = match fields {
        Value::Object(o) => o,
        _ => Map::new(),
    };
    out.insert("v".to_string(), json!(EV_VERSION));
    out.insert("t".to_string(), json!(t));
    Value::Object(out)
}

/// Merge an event into a milestone's `data` under [`EV_KEY`], leaving every
/// domain key exactly as it was.
fn with_ev(event: Value, data: Value) -> Value {
    let mut out = match data {
        Value::Object(o) => o,
        _ => Map::new(),
    };
    out.insert(EV_KEY.to_string(), event);
    Value::Object(out)
}

/// Print one tagged milestone. `data` is the machine-readable half; the
/// message is what an operator tailing the console reads.
pub fn print(host: &dyn Host, tag: &str, message: &str, data: Value) {
    let _ = host.exec(
        CONSOLE_PRINT_REF,
        &json!({ "level": "info", "message": format!("[{tag}] {message}"), "data": data }),
    );
}

/// Same, at `warn` — used for a degraded-but-continuing outcome: a look-up that
/// failed while others succeeded, or a step dropped from an otherwise usable
/// plan.
pub fn warn(host: &dyn Host, tag: &str, message: &str, data: Value) {
    let _ = host.exec(
        CONSOLE_PRINT_REF,
        &json!({ "level": "warn", "message": format!("[{tag}] {message}"), "data": data }),
    );
}

/// [`print`], with a progress event merged into `data`. The message keeps its
/// `[{tag}] {message}` shape exactly, so every renderer that reads `message`
/// (the CLI stderr echo, solx-mcp progress notifications, solx-web's console
/// pane) is unaffected.
pub fn print_ev(host: &dyn Host, tag: &str, message: &str, event: Value, data: Value) {
    print(host, tag, message, with_ev(event, data));
}

/// [`warn`], with a progress event merged into `data`.
pub fn warn_ev(host: &dyn Host, tag: &str, message: &str, event: Value, data: Value) {
    warn(host, tag, message, with_ev(event, data));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_parseable_and_stable() {
        assert_eq!(phase_tag(PHASE_INTENT), "prompt:intent");
        assert_eq!(phase_tag(PHASE_SEARCH), "prompt:search");
        assert_eq!(phase_tag(PHASE_STEPS), "prompt:steps");
        assert_eq!(phase_step_tag(PHASE_INTENT, STEP_START), "prompt:intent:start");
        assert_eq!(phase_step_tag(PHASE_SEARCH, STEP_HITS), "prompt:search:hits");
        assert_eq!(phase_step_tag(PHASE_RUN, STEP_CANCELLED), "prompt:run:cancelled");
    }

    #[test]
    fn a_phase_step_tag_is_not_a_prefix_match_for_its_bare_phase() {
        // What makes `[prompt:intent]` unambiguous for a consumer matching on
        // the bracketed prefix.
        let stepped = format!("[{}]", phase_step_tag(PHASE_INTENT, STEP_START));
        assert!(!stepped.starts_with(&format!("[{}]", phase_tag(PHASE_INTENT))));
    }

    #[test]
    fn an_event_carries_its_version_and_type() {
        let e = ev(EV_SEARCH_DONE, json!({ "hits": 7 }));
        assert_eq!(e.get("v").and_then(Value::as_u64), Some(EV_VERSION));
        assert_eq!(e.get("t").and_then(Value::as_str), Some(EV_SEARCH_DONE));
        assert_eq!(e.get("hits").and_then(Value::as_u64), Some(7));
    }

    #[test]
    fn merging_an_event_leaves_every_domain_key_intact() {
        let data = json!({ "count": 7, "refs": ["/notes/auth"] });
        let merged = with_ev(ev(EV_SEARCH_DONE, json!({ "hits": 7 })), data);
        assert_eq!(merged.get("count").and_then(Value::as_u64), Some(7));
        assert!(merged.get("refs").is_some());
        assert_eq!(
            merged.get(EV_KEY).and_then(|e| e.get("t")).and_then(Value::as_str),
            Some(EV_SEARCH_DONE),
        );
        // Exactly one key added, never more.
        assert_eq!(merged.as_object().unwrap().len(), 3);
    }

    #[test]
    fn a_non_object_payload_still_carries_its_event() {
        let merged = with_ev(ev(EV_RUN_STARTED, Value::Null), Value::Null);
        assert_eq!(
            merged.get(EV_KEY).and_then(|e| e.get("t")).and_then(Value::as_str),
            Some(EV_RUN_STARTED),
        );
    }

    #[test]
    fn every_event_name_is_namespaced_by_a_phase_this_pipeline_has() {
        // A consumer folding these switches on `ev.t` and groups by the part
        // before the dot, so an event naming a phase that does not exist here
        // would be unplaceable. This is what keeps the flat vocabulary honest
        // as events are added.
        let phases = [
            PHASE_RUN, PHASE_RECALL, PHASE_CONTEXT, PHASE_INTENT, PHASE_SEARCH, PHASE_STEPS,
        ];
        for name in [
            EV_RUN_STARTED,
            EV_RUN_DONE,
            EV_RUN_FAILED,
            EV_RUN_CANCELLED,
            EV_RECALL_DONE,
            EV_CONTEXT_DONE,
            EV_INTENT_STARTED,
            EV_INTENT_DONE,
            EV_INTENT_FAILED,
            EV_SEARCH_STARTED,
            EV_SEARCH_DONE,
            EV_STEPS_STARTED,
            EV_STEPS_DONE,
            EV_STEPS_FAILED,
        ] {
            let phase = name.split('.').next().unwrap();
            assert!(phases.contains(&phase), "{name} names no known phase");
        }
    }
}

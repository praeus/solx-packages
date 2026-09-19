//! Tagged console output for `multi_inquire`.
//!
//! Every milestone of a `multi_inquire` run is printed to the action's own
//! console with **both** a human-readable tagged message and a
//! machine-readable `data` object, so a caller can reconstruct the run
//! afterwards from `/builtin/console/read` without re-running anything. The
//! point of the tag grammar is that it is parseable: `[multi_inquire:<phase>]`
//! or `[multi_inquire:inquiry:<index>:<step>]`, one vocabulary, defined here
//! and nowhere else.
//!
//! On top of that, every milestone carries a **progress event** under the
//! reserved `data.ev` key — see [`ev`]. The tag grammar is the human
//! affordance; the envelope is the wire format. A UI that wants to render
//! progress switches on `ev.t` and never has to regex prose. Two reasons it
//! lives *beside* the existing payload rather than replacing it: the current
//! `data` shapes are a declared contract (above), and prefixes nest —
//! `console/copy` prepends its own `[label]` to an already tagged message, so
//! the first bracketed group in a message is not reliably the tag.
//!
//! Printing is best-effort throughout. A console hiccup is an observability
//! problem, never a reason to fail a pipeline that is otherwise producing
//! correct results — the same contract `llm::drain_console` already keeps. The
//! corollary binds every *consumer*: any single event can be lost, so nothing
//! may require that event A arrived before event B is understood.

use serde_json::{json, Map, Value};

use crate::host::Host;
use crate::llm::CONSOLE_PRINT_REF;

/// Root of every tag this package prints. A caller filtering console entries
/// for a `multi_inquire` run matches on this prefix.
pub const TAG_ROOT: &str = "multi_inquire";

pub const PHASE_RUN: &str = "run";
pub const PHASE_RECALL: &str = "recall";
pub const PHASE_CONTEXT: &str = "context";
pub const PHASE_INTENT: &str = "intent";
pub const PHASE_FANOUT: &str = "fanout";
pub const PHASE_RESULT: &str = "result";

pub const STEP_TERMS: &str = "terms";
pub const STEP_HITS: &str = "hits";
pub const STEP_START: &str = "start";
pub const STEP_DONE: &str = "done";
pub const STEP_RESULT: &str = "result";
pub const STEP_ERROR: &str = "error";
pub const STEP_CANCELLED: &str = "cancelled";
pub const STEP_DEGRADED: &str = "degraded";

/// `[multi_inquire:<phase>]`.
pub fn phase_tag(phase: &str) -> String {
    format!("{TAG_ROOT}:{phase}")
}

/// `[multi_inquire:<phase>:<step>]` — the phase-level counterpart of
/// [`inquiry_step_tag`].
///
/// A phase that prints more than once needs it: `intent` now reports both its
/// start and its decision, and a consumer matching on the bare
/// `[multi_inquire:intent]` prefix would otherwise pick up whichever came
/// first. One tag, one kind of print, at every level of the grammar.
pub fn phase_step_tag(phase: &str, step: &str) -> String {
    format!("{}:{}", phase_tag(phase), step)
}

/// `[multi_inquire:inquiry:<index>]` — also the prefix echoed child console
/// lines carry, which is what makes live model output attributable to one
/// inquiry.
pub fn inquiry_tag(index: usize) -> String {
    format!("{TAG_ROOT}:inquiry:{index}")
}

/// `[multi_inquire:inquiry:<index>:<step>]`.
pub fn inquiry_step_tag(index: usize, step: &str) -> String {
    format!("{}:{}", inquiry_tag(index), step)
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
/// with a domain key — `intent.to_json()` already has a `mode`, and an event
/// wants one too.
pub const EV_KEY: &str = "ev";

/// How much of an inquiry's question rides along as `ev.q`.
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
pub const EV_INQUIRY_PLANNED: &str = "inquiry.planned";
pub const EV_INQUIRY_SEARCHED: &str = "inquiry.searched";
pub const EV_INQUIRY_STARTED: &str = "inquiry.started";
pub const EV_INQUIRY_FINISHED: &str = "inquiry.finished";
pub const EV_INQUIRY_FAILED: &str = "inquiry.failed";
pub const EV_INQUIRY_RESULT: &str = "inquiry.result";
pub const EV_FANOUT_STARTED: &str = "fanout.started";
pub const EV_FANOUT_DEGRADED: &str = "fanout.degraded";

/// Build one progress event: `{v, t, ...fields}`.
///
/// Field names are short (`i`, `n`, `q`, `kind`, `ok`) for the same reason
/// [`Q_CAP`] exists.
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

/// Same, at `warn` — used for a degraded-but-continuing outcome (a failed
/// inquiry among several that succeeded).
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
        assert_eq!(phase_tag(PHASE_INTENT), "multi_inquire:intent");
        assert_eq!(phase_tag(PHASE_FANOUT), "multi_inquire:fanout");
        assert_eq!(inquiry_tag(0), "multi_inquire:inquiry:0");
        assert_eq!(inquiry_step_tag(2, STEP_HITS), "multi_inquire:inquiry:2:hits");
        assert_eq!(inquiry_step_tag(1, STEP_START), "multi_inquire:inquiry:1:start");
        assert_eq!(inquiry_step_tag(1, STEP_DONE), "multi_inquire:inquiry:1:done");
        assert_eq!(phase_step_tag(PHASE_INTENT, STEP_START), "multi_inquire:intent:start");
        assert_eq!(phase_step_tag(PHASE_RUN, STEP_CANCELLED), "multi_inquire:run:cancelled");
    }

    #[test]
    fn a_phase_step_tag_is_not_a_prefix_match_for_its_bare_phase() {
        // What makes `[multi_inquire:intent]` unambiguous for a consumer
        // matching on the bracketed prefix.
        let stepped = format!("[{}]", phase_step_tag(PHASE_INTENT, STEP_START));
        assert!(!stepped.starts_with(&format!("[{}]", phase_tag(PHASE_INTENT))));
    }

    #[test]
    fn an_event_carries_its_version_and_type() {
        let e = ev(EV_INQUIRY_STARTED, json!({ "i": 2, "kind": "documents" }));
        assert_eq!(e.get("v").and_then(Value::as_u64), Some(EV_VERSION));
        assert_eq!(e.get("t").and_then(Value::as_str), Some(EV_INQUIRY_STARTED));
        assert_eq!(e.get("i").and_then(Value::as_u64), Some(2));
    }

    #[test]
    fn merging_an_event_leaves_every_domain_key_intact() {
        let data = json!({ "count": 7, "refs": ["/notes/auth"] });
        let merged = with_ev(ev(EV_INQUIRY_SEARCHED, json!({ "i": 0 })), data);
        assert_eq!(merged.get("count").and_then(Value::as_u64), Some(7));
        assert!(merged.get("refs").is_some());
        assert_eq!(
            merged.get(EV_KEY).and_then(|e| e.get("t")).and_then(Value::as_str),
            Some(EV_INQUIRY_SEARCHED),
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
}

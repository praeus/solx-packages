//! Host-target tests for `dispatch` itself: how a call is routed, and how a
//! malformed one is refused.
//!
//! These need no host at all — the pipeline reaches `exec` only once a phase
//! exists, and every case here is decided before that. The shared `FakeHost`
//! the phase tests use lives in `fake_host.rs`.

use serde_json::{json, Value};

use solx_prompt::host::{Host, HostCall, Outcome};
use solx_prompt::{dispatch, PROMPT_FN};

/// A host that refuses every call, so a test that unexpectedly starts reaching
/// the outside world fails loudly here rather than quietly succeeding.
struct NoHost;

impl Host for NoHost {
    fn exec(&self, action_ref: &str, _payload: &Value) -> Result<HostCall, String> {
        panic!("no exec expected in a dispatch test, got {action_ref}");
    }
    fn log(&self, _msg: &str) {}
}

fn run(params: &str) -> Outcome {
    dispatch(&NoHost, Some(PROMPT_FN), params)
}

// ── routing ──────────────────────────────────────────────────────────────────

#[test]
fn a_missing_fn_name_names_what_it_would_have_accepted() {
    let out = dispatch(&NoHost, None, "{}");
    assert!(!out.success);
    assert_eq!(out.output["kind"], json!("unknown_action"));
    assert_eq!(out.output["known"], json!([PROMPT_FN]));
}

#[test]
fn an_unknown_fn_name_is_refused_and_echoed() {
    let out = dispatch(&NoHost, Some("multi_inquire"), "{}");
    assert!(!out.success);
    assert_eq!(out.output["kind"], json!("unknown_action"));
    assert_eq!(out.output["fn_name"], json!("multi_inquire"));
}

// ── params ───────────────────────────────────────────────────────────────────

#[test]
fn params_that_are_not_json_are_refused_with_the_parse_error() {
    let out = run("{not json");
    assert!(!out.success);
    assert_eq!(out.output["kind"], json!("bad_params"));
    assert!(out.message.unwrap().contains("not valid JSON"));
}

#[test]
fn params_that_are_json_but_not_an_object_are_refused() {
    for body in ["[]", "\"prompt\"", "7", "true"] {
        let out = run(body);
        assert!(!out.success, "{body} should not be accepted");
        assert_eq!(out.output["kind"], json!("bad_params"), "{body}");
    }
}

#[test]
fn an_empty_params_string_is_read_as_no_arguments_not_as_a_parse_error() {
    // `null` is how a caller with nothing to pass spells it. It must fail on the
    // *missing required field*, not on parsing - otherwise the error sends
    // someone looking at their JSON rather than at their arguments.
    let out = run("null");
    assert!(!out.success);
    assert_eq!(out.output["kind"], json!("bad_params"));
    assert!(out.message.unwrap().contains("prompt is required"));
}

#[test]
fn prompt_and_model_are_both_required_and_must_be_non_empty() {
    for (params, expected) in [
        (json!({}), "prompt is required"),
        (json!({ "prompt": "  " , "model": "m" }), "prompt is required"),
        (json!({ "prompt": "do a thing" }), "model is required"),
        (json!({ "prompt": "do a thing", "model": "" }), "model is required"),
    ] {
        let out = run(&params.to_string());
        assert!(!out.success, "{params} should not be accepted");
        assert!(out.message.unwrap().contains(expected), "{params}");
    }
}

#[test]
fn params_are_accepted_in_either_snake_or_camel_case() {
    // `normalize_params` aliases the two, so a caller reaching this action
    // through a layer that camelCases its keys is not silently ignored.
    let out = run(&json!({ "prompt": "do a thing", "model": "m", "maxResults": 3 }).to_string());
    assert!(out.success, "{:?}", out.message);
}

// ── the result envelope ──────────────────────────────────────────────────────

#[test]
fn the_envelope_carries_every_declared_key_even_when_a_phase_produced_nothing() {
    // The shape is the contract the widget codes against, so it must not depend
    // on whether a phase found anything. This is what lets the widget be built
    // against the scaffold and keep working as the phases land.
    let out = run(&json!({ "prompt": "do a thing", "model": "qwen3:4b" }).to_string());
    assert!(out.success, "{:?}", out.message);

    assert_eq!(out.output["prompt"], json!("do a thing"));
    assert_eq!(out.output["model"], json!("qwen3:4b"));
    for key in [
        "mode", "message", "steps", "destructive", "next_prompt", "memories", "turn", "hits",
        "notes", "errors",
    ] {
        assert!(out.output.get(key).is_some(), "envelope is missing {key}");
    }
    for key in ["steps", "destructive", "memories", "hits", "notes", "errors"] {
        assert!(out.output[key].is_array(), "{key} must be an array, empty or not");
    }
}

#[test]
fn the_turn_record_carries_no_results_because_the_steps_have_not_run() {
    // `results` is the caller's to append after it executes the steps - this
    // action never sees them. A `results: []` here would read as "they ran and
    // produced nothing", which is a different claim.
    let out = run(&json!({ "prompt": "do a thing", "model": "m" }).to_string());
    let turn = &out.output["turn"];
    assert!(turn.get("results").is_none(), "turn must not claim results: {turn}");
    assert_eq!(turn["prompt"], json!("do a thing"));
}

// ── vendored wit ─────────────────────────────────────────────────────────────

#[test]
fn vendored_wit_matches_solx_core_when_present() {
    // solx-packages is a separate repo, so the sibling checkout may not exist;
    // this only guards a dev machine that has both. One `..` deeper than
    // solx-inquiry's equivalent, because the crate sits under crate/.
    let sibling = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../solx-core/solx-wasm/wit/custom-action.wit");
    let Ok(upstream) = std::fs::read_to_string(&sibling) else {
        return;
    };
    assert_eq!(
        upstream.replace("\r\n", "\n"),
        include_str!("../wit/custom-action.wit").replace("\r\n", "\n"),
        "vendored wit/custom-action.wit has drifted from solx-core"
    );
}

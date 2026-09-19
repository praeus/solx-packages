//! Pipeline tests.
//!
//! Scaffold: the three phases do not exist yet, so what this asserts is the
//! machinery they will run on — that a scripted llm call is driven correctly
//! end to end, and that the invariants the result envelope promises hold. Each
//! phase adds its own section here as it lands.

mod fake_host;

use serde_json::json;

use fake_host::{FakeHost, ACTION_CANCELLED, ACTION_POLL, ACTION_START, LLM_REF};
use solx_prompt::host::Outcome;
use solx_prompt::llm;
use solx_prompt::params::Params;

fn run(host: &FakeHost, params: serde_json::Value) -> Outcome {
    solx_prompt::dispatch(host, Some(solx_prompt::PROMPT_FN), &params.to_string())
}

fn base_params() -> serde_json::Value {
    json!({
        "prompt": "how does auth work, and how do I search for it?",
        "model": "qwen3:4b",
        "max_results": 3,
    })
}

// ── the llm plumbing both phases run on ──────────────────────────────────────

#[test]
fn a_detached_llm_call_starts_polls_and_returns_the_models_content() {
    let host = FakeHost::new();
    host.push_llm_call("inv-intent", r#"{"message":"hello"}"#);

    let result = llm::call(&host, &Params::default(), json!({ "model": "m" }), "intent")
        .expect("the scripted call should succeed");

    assert_eq!(result.pointer("/message/content").unwrap(), &json!(r#"{"message":"hello"}"#));

    // The detached path, in order: start, then check our own cancellation
    // before committing to a long poll, then poll.
    let refs = host.refs();
    let start = refs.iter().position(|r| r == ACTION_START).expect("no action/start");
    let cancelled = refs.iter().position(|r| r == ACTION_CANCELLED).expect("no action/cancelled");
    let poll = refs.iter().position(|r| r == ACTION_POLL).expect("no action/poll");
    assert!(start < cancelled, "cancellation is checked after starting: {refs:?}");
    assert!(cancelled < poll, "cancellation is checked before polling: {refs:?}");
}

#[test]
fn a_host_that_is_not_long_lived_falls_back_to_a_blocking_call() {
    // `action/start` is refused under a bare `solx exec`, because the process
    // exits the moment exec returns and would kill the spawned task. The phase
    // must still produce an answer.
    let host = FakeHost::new();
    host.push_err(ACTION_START, "detached invocations require a long-lived host");
    host.push_llm_blocking(r#"{"message":"answered anyway"}"#);

    let result = llm::call(&host, &Params::default(), json!({ "model": "m" }), "intent")
        .expect("the blocking fallback should succeed");

    assert_eq!(
        result.pointer("/message/content").unwrap(),
        &json!(r#"{"message":"answered anyway"}"#)
    );
    assert_eq!(host.count(LLM_REF), 1, "the chat action should be called directly once");
    assert!(host.never_called(ACTION_POLL), "nothing to poll on the blocking path");
}

#[test]
fn a_failed_llm_call_reports_the_stage_it_failed_in() {
    // Both phases share every `/builtin/action/*` ref, so the stage is the only
    // thing in the error that says which one failed.
    let host = FakeHost::new();
    host.push_err(ACTION_START, "detached invocations require a long-lived host");
    host.push_fail(LLM_REF, "model not found");

    let outcome = llm::call(&host, &Params::default(), json!({ "model": "m" }), "steps")
        .expect_err("a failed chat call should not be reported as success");

    assert_eq!(outcome.output["stage"], json!("steps"));
    assert!(!outcome.success);
}

#[test]
fn the_configured_llm_action_is_the_one_called() {
    // `llm_action_ref` is the whole extension point: any action taking
    // {model, messages, format?} can stand in for ollama-chat.
    let host = FakeHost::new();
    let params = Params { llm_action_ref: "/packages/other/chat".to_string(), ..Params::default() };
    host.push_err(ACTION_START, "detached invocations require a long-lived host");
    host.push_ok(
        "/packages/other/chat",
        json!({ "message": { "content": "{}" }, "done": true }),
    );

    llm::call(&host, &params, json!({ "model": "m" }), "intent").expect("should call the override");

    assert!(host.never_called(LLM_REF), "the default chat action should not be touched");
    assert_eq!(host.count("/packages/other/chat"), 1);
}

// ── invariants of the action itself ──────────────────────────────────────────

#[test]
fn the_action_never_saves_a_document() {
    // The invariant that keeps the thing producing model output from also being
    // the thing that records it. With no session ref in the params this is
    // nearly structural — the crate holds no reference to entity-save-document
    // at all — but asserting it here is what will catch a phase reaching for it
    // later.
    let host = FakeHost::new();
    let out = run(&host, base_params());
    assert!(out.success, "{:?}", out.message);

    for reference in host.refs() {
        assert!(
            !reference.contains("entity-save-document"),
            "the prompt action saved a document: {reference}"
        );
        assert!(
            !reference.contains("entity-delete-document"),
            "the prompt action deleted a document: {reference}"
        );
    }
}

#[test]
fn a_turn_needs_no_session_reference_to_produce_a_record() {
    // Dropping the session ref is what lets the caller own history; the turn
    // record has to come back regardless, because it is the thing the caller
    // appends to whatever it keeps.
    let host = FakeHost::new();
    let out = run(&host, base_params());
    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["turn"]["prompt"], base_params()["prompt"]);
    assert!(out.output.get("session").is_none(), "there is no session ref to echo");
    assert!(out.output.get("session_document").is_none(), "the caller builds the document");
}

//! Host-target tests for the whole `inquire` pipeline.
//!
//! `FakeHost` replays canned `exec` responses per `action_ref` (a separate
//! FIFO queue for each), and records every call made, so the exact sequence
//! of nested calls can be asserted without a network, an Ollama server, a
//! solx-server, or a wasm runtime. Keying by `action_ref` rather than one
//! flat queue matters here specifically: every llm call now drives several
//! distinct `/builtin/action/*` and `/builtin/console/*` calls (see
//! `src/llm.rs`), and the *same* refs are reused across both the terms and
//! the summary phase — a single flat queue would silently misattribute a
//! response meant for one phase/ref to a call on a different one the moment
//! a test's call count guess was off by one, rather than failing loudly.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use serde_json::{json, Value};

use solx_inquiry::host::{Host, HostCall, Outcome};
use solx_inquiry::search::{ACTION_SEARCH_REF, DOCUMENT_SEARCH_REF, TYPE_GET_REF};

const LLM_REF: &str = "/packages/solx-ollama/ollama-chat";
const ACTION_START: &str = "/builtin/action/start";
const ACTION_POLL: &str = "/builtin/action/poll";
const ACTION_STOP: &str = "/builtin/action/stop";
const ACTION_CANCELLED: &str = "/builtin/action/cancelled";
const CONSOLE_TAIL: &str = "/builtin/console/tail";
const CONSOLE_COPY: &str = "/builtin/console/copy";

struct FakeHost {
    calls: RefCell<Vec<(String, Value)>>,
    responses: RefCell<HashMap<String, VecDeque<Result<HostCall, String>>>>,
}

impl FakeHost {
    fn new() -> Self {
        FakeHost {
            calls: RefCell::new(Vec::new()),
            responses: RefCell::new(HashMap::new()),
        }
    }

    fn push_ok(&self, action_ref: &str, result: Value) -> &Self {
        self.responses
            .borrow_mut()
            .entry(action_ref.to_string())
            .or_default()
            .push_back(Ok(HostCall { success: true, message: None, result }));
        self
    }

    fn push_fail(&self, action_ref: &str, message: &str) -> &Self {
        self.responses
            .borrow_mut()
            .entry(action_ref.to_string())
            .or_default()
            .push_back(Ok(HostCall {
                success: false,
                message: Some(message.to_string()),
                result: Value::Null,
            }));
        self
    }

    fn push_err(&self, action_ref: &str, message: &str) -> &Self {
        self.responses
            .borrow_mut()
            .entry(action_ref.to_string())
            .or_default()
            .push_back(Err(message.to_string()));
        self
    }

    /// Queue one complete detached llm call that finishes on its very first
    /// poll: `action/start`, `action/cancelled` (false), `action/poll`
    /// (`ok`, carrying `content`), then a drain (`console/tail` +
    /// `console/copy` — see [`Self::push_tail_with_entries`]).
    fn push_llm_call_detached(&self, invocation_id: &str, content: &str, entries: Vec<Value>) -> &Self {
        self.push_ok(
            ACTION_START,
            json!({ "invocation_id": invocation_id, "action_ref": LLM_REF, "console_seq_start": 0 }),
        );
        self.push_ok(ACTION_CANCELLED, json!({ "cancelled": false }));
        self.push_ok(
            ACTION_POLL,
            json!({
                "invocation_id": invocation_id, "action_ref": LLM_REF, "status": "ok",
                "result": { "message": { "role": "assistant", "content": content }, "done": true },
                "error": Value::Null,
            }),
        );
        self.push_tail_with_entries(invocation_id, entries);
        self
    }

    /// Same as [`Self::push_llm_call_detached`] with no console output.
    fn push_llm_call_detached_quiet(&self, invocation_id: &str, content: &str) -> &Self {
        self.push_llm_call_detached(invocation_id, content, vec![])
    }

    /// Queue a detached llm call that reports `running` on its first poll
    /// (draining `mid_entries` meanwhile), then completes on the second.
    fn push_llm_call_detached_two_polls(&self, invocation_id: &str, mid_entries: Vec<Value>, content: &str) -> &Self {
        self.push_ok(
            ACTION_START,
            json!({ "invocation_id": invocation_id, "action_ref": LLM_REF, "console_seq_start": 0 }),
        );
        self.push_ok(ACTION_CANCELLED, json!({ "cancelled": false }));
        self.push_ok(
            ACTION_POLL,
            json!({ "invocation_id": invocation_id, "action_ref": LLM_REF, "status": "running", "result": Value::Null, "error": Value::Null }),
        );
        self.push_tail_with_entries(invocation_id, mid_entries);
        self.push_ok(ACTION_CANCELLED, json!({ "cancelled": false }));
        self.push_ok(
            ACTION_POLL,
            json!({
                "invocation_id": invocation_id, "action_ref": LLM_REF, "status": "ok",
                "result": { "message": { "role": "assistant", "content": content }, "done": true },
                "error": Value::Null,
            }),
        );
        self.push_tail_with_entries(invocation_id, vec![]);
        self
    }

    /// Queue the fallback path: `action/start` refused for lacking a
    /// long-lived host, then a plain blocking `exec` of the llm action ref
    /// itself.
    fn push_llm_call_fallback(&self, content: &str) -> &Self {
        self.push_err(ACTION_START, "action_start requires a long-lived host (solx-server or solx-mcp).");
        self.push_ok(LLM_REF, json!({ "message": { "role": "assistant", "content": content }, "done": true }))
    }

    /// One `console/tail` response, plus one `console/copy` response — one
    /// call, whatever the count, mirroring `src/llm.rs::drain_console`'s own
    /// shape: it copies every tracked invocation's new entries in a single
    /// `console/copy` regardless of how many there are, rather than one
    /// `console/print` per entry.
    fn push_tail_with_entries(&self, invocation_id_filter: &str, entries: Vec<Value>) -> &Self {
        let matching = entries
            .iter()
            .filter(|e| e.get("invocation_id").and_then(Value::as_str) == Some(invocation_id_filter))
            .count();
        let next_cursor = entries.len() as i64;
        self.push_ok(CONSOLE_TAIL, json!({ "entries": entries, "next_cursor": next_cursor, "first_seq": 0, "dropped": 0 }));
        self.push_ok(CONSOLE_COPY, json!({ "copied": matching, "next_cursor": matching as i64 }));
        self
    }

    fn call_names(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|(n, _)| n.clone()).collect()
    }

    fn calls_named(&self, name: &str) -> Vec<Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

impl Host for FakeHost {
    fn exec(&self, action_ref: &str, payload: &Value) -> Result<HostCall, String> {
        self.calls.borrow_mut().push((action_ref.to_string(), payload.clone()));
        self.responses
            .borrow_mut()
            .get_mut(action_ref)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| panic!("FakeHost ran out of responses at {action_ref}"))
    }

    fn log(&self, _msg: &str) {}
}

fn run(host: &FakeHost, params: Value) -> Outcome {
    solx_inquiry::dispatch(host, Some("inquire"), &params.to_string())
}

fn kind(outcome: &Outcome) -> String {
    outcome.output.get("kind").and_then(Value::as_str).unwrap_or("<none>").to_string()
}

fn base_params() -> Value {
    json!({ "inquiry": "how does auth work here?", "model": "qwen3:4b" })
}

fn console_entry(invocation_id: &str, message: &str) -> Value {
    json!({ "seq": 1, "ts": "2026-01-01T00:00:00Z", "level": "chunk", "invocation_id": invocation_id, "run_id": Value::Null, "source": "guest", "message": message, "data": Value::Null })
}

// ── happy path, detached ─────────────────────────────────────────────────────

#[test]
fn full_pipeline_documents_scope_via_detached_llm_calls() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["authentication", "session token"]}"#);
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({
            "items": [{ "id": "1", "path": "/notes", "name": "auth", "title": "Auth notes", "summary": "how login works", "typeRef": "/types/core/Object", "contents": { "body": "session tokens are issued at login" } }],
            "total": 1, "limit": 10, "offset": 0,
        }),
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "Authentication uses session tokens, per /notes/auth.");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["terms"], json!(["authentication", "session token"]));
    assert_eq!(out.output["hits"][0]["path"], json!("/notes"));
    // `contents` comes back inline from search_documents - no separate
    // entity_get_document round trip needed.
    assert_eq!(out.output["hits"][0]["details"], json!({ "body": "session tokens are issued at login" }));
    assert_eq!(out.output["summary"], json!("Authentication uses session tokens, per /notes/auth."));
    assert!(!host.call_names().contains(&"/builtin/document/entity_get_document".to_string()));

    assert_eq!(host.calls_named(ACTION_START).len(), 2, "one detached start per llm phase");
    assert_eq!(host.calls_named(DOCUMENT_SEARCH_REF).len(), 2);
    assert!(host.calls_named(ACTION_SEARCH_REF).is_empty());
}

#[test]
fn console_output_from_the_detached_call_is_drained_via_console_copy() {
    // The actual filtering, renumbering and message-prefixing now happen
    // server-side in solx-console's own console/copy (see its own tests) -
    // what this pipeline is responsible for is calling it with the right
    // shape: the child's own action_ref and invocation_id, this call's stage
    // as the label, and a cursor to resume from.
    let host = FakeHost::new();
    host.push_llm_call_detached(
        "inv-terms",
        r#"{"terms": ["auth"]}"#,
        vec![console_entry("inv-terms", "thinking about it")],
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "no results");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    let copies = host.calls_named(CONSOLE_COPY);
    assert_eq!(copies.len(), 2, "one per stage: terms, summary - {copies:?}");
    assert_eq!(copies[0]["from_action_ref"], json!(LLM_REF));
    assert_eq!(copies[0]["invocation_id"], json!("inv-terms"));
    assert_eq!(copies[0]["label"], json!("terms"));
    assert_eq!(copies[0]["cursor"], json!(0));
}

#[test]
fn console_copy_names_this_calls_own_invocation_not_a_concurrent_callers() {
    // The child action's console is shared across every concurrent caller
    // (keyed by action_ref alone) - console/copy's own invocation_id filter
    // is what keeps a line from some other invocation out of this call's
    // console (see solx-console's own tests for that filter). What this
    // pipeline must get right on its side is naming *this* call's
    // invocation_id, not some other one merely visible on the same tail.
    let host = FakeHost::new();
    host.push_llm_call_detached(
        "inv-terms",
        r#"{"terms": ["auth"]}"#,
        vec![
            console_entry("some-other-concurrent-call", "not mine"),
            console_entry("inv-terms", "mine"),
        ],
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "no results");

    run(&host, base_params());

    let copies = host.calls_named(CONSOLE_COPY);
    assert_eq!(copies[0]["invocation_id"], json!("inv-terms"));
}

#[test]
fn a_still_running_poll_loops_and_keeps_draining_console_until_terminal() {
    let host = FakeHost::new();
    host.push_llm_call_detached_two_polls(
        "inv-terms",
        vec![console_entry("inv-terms", "still working")],
        r#"{"terms": ["auth"]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "no results");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["terms"], json!(["auth"]));
    assert_eq!(host.calls_named(ACTION_POLL).len(), 3, "2 for terms + 1 for summary");
    // Drained once per poll of the still-running terms call, plus once for
    // the summary call: three drains, not one print per entry.
    let copies = host.calls_named(CONSOLE_COPY);
    assert_eq!(copies.len(), 3, "{copies:?}");
}

// ── fallback (non-long-lived host) ──────────────────────────────────────────

#[test]
fn falls_back_to_a_blocking_call_when_the_host_cannot_detach() {
    let host = FakeHost::new();
    host.push_llm_call_fallback(r#"{"terms": ["auth"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_fallback("no results");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["terms"], json!(["auth"]));
    // Fell all the way through to a plain exec of the llm ref itself.
    assert!(host.call_names().contains(&LLM_REF.to_string()));
    assert!(host.calls_named(ACTION_POLL).is_empty());
}

#[test]
fn action_start_failure_for_any_other_reason_is_a_dispatch_error() {
    let host = FakeHost::new();
    host.push_err(ACTION_START, "no such action /packages/solx-ollama/ollama-chat");

    let out = run(&host, base_params());

    assert!(!out.success);
    assert_eq!(kind(&out), "dispatch_error");
    assert_eq!(out.output["stage"], json!("terms"));
    // Must not have silently fallen back to a blocking exec after a genuine
    // dispatch failure that has nothing to do with host lifetime.
    assert!(!host.call_names().contains(&LLM_REF.to_string()));
}

// ── cooperative cancellation ─────────────────────────────────────────────────

#[test]
fn cancellation_of_inquires_own_invocation_stops_the_child_and_aborts() {
    let host = FakeHost::new();
    host.push_ok(ACTION_START, json!({ "invocation_id": "inv-terms", "action_ref": LLM_REF, "console_seq_start": 0 }));
    host.push_ok(ACTION_CANCELLED, json!({ "cancelled": true }));
    host.push_ok(ACTION_STOP, json!({ "invocation_id": "inv-terms", "status": "cancelling" }));

    let out = run(&host, base_params());

    assert!(!out.success);
    assert_eq!(kind(&out), "cancelled");
    assert_eq!(out.output["stage"], json!("terms"));

    let stop = host.calls_named(ACTION_STOP);
    assert_eq!(stop.len(), 1);
    assert_eq!(stop[0]["invocation_id"], json!("inv-terms"));
    // Cancellation is checked before polling, so a call cancelled on its
    // first check never polls at all.
    assert!(host.calls_named(ACTION_POLL).is_empty());
}

// ── term parsing fallbacks reach the pipeline too ───────────────────────────

#[test]
fn a_model_that_ignores_format_and_answers_with_a_plain_list_still_works() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", "- authentication\n- session token");
    // One search_documents call per parsed term.
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "no results");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["terms"], json!(["authentication", "session token"]));
}

#[test]
fn unparseable_term_output_fails_with_bad_llm_output() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", "");

    let out = run(&host, base_params());

    assert!(!out.success);
    assert_eq!(kind(&out), "bad_llm_output");
    assert_eq!(out.output["stage"], json!("terms"));
    assert!(host.calls_named(DOCUMENT_SEARCH_REF).is_empty(), "must not search with no terms");
}

// ── params ───────────────────────────────────────────────────────────────────

#[test]
fn missing_inquiry_or_model_is_bad_params() {
    let host = FakeHost::new();

    let out = run(&host, json!({ "model": "m" }));
    assert_eq!(kind(&out), "bad_params");
    assert_eq!(out.output["missing"], json!(["inquiry"]));

    let out = run(&host, json!({ "inquiry": "q" }));
    assert_eq!(kind(&out), "bad_params");
    assert_eq!(out.output["missing"], json!(["model"]));

    assert!(host.call_names().is_empty(), "a param failure must not reach any nested action");
}

#[test]
fn invalid_scope_is_bad_params() {
    let host = FakeHost::new();
    let mut params = base_params();
    params["scope"] = json!("everything");

    let out = run(&host, params);
    assert_eq!(kind(&out), "bad_params");
}

#[test]
fn max_terms_is_forwarded_to_the_format_schema() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["a"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "s");

    let mut params = base_params();
    params["max_terms"] = json!(2);
    run(&host, params);

    let start_calls = host.calls_named(ACTION_START);
    assert_eq!(start_calls[0]["params"]["format"]["properties"]["terms"]["maxItems"], json!(2));
}

#[test]
fn llm_action_ref_is_overridable() {
    let host = FakeHost::new();
    const CUSTOM: &str = "/packages/solx-custom-llm/chat";
    host.push_ok(ACTION_START, json!({ "invocation_id": "inv-1", "action_ref": CUSTOM, "console_seq_start": 0 }));
    host.push_ok(ACTION_CANCELLED, json!({ "cancelled": false }));
    host.push_ok(
        ACTION_POLL,
        json!({
            "invocation_id": "inv-1", "action_ref": CUSTOM, "status": "ok",
            "result": { "message": { "content": r#"{"terms": ["a"]}"# } }, "error": Value::Null,
        }),
    );
    host.push_ok(CONSOLE_TAIL, json!({ "entries": [], "next_cursor": 0, "first_seq": 0, "dropped": 0 }));
    host.push_ok(CONSOLE_COPY, json!({ "copied": 0, "next_cursor": 0 }));
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-2", "s");

    let mut params = base_params();
    params["llm_action_ref"] = json!(CUSTOM);
    run(&host, params);

    let start_calls = host.calls_named(ACTION_START);
    assert_eq!(start_calls[0]["path"], json!("/packages/solx-custom-llm"));
    assert_eq!(start_calls[0]["name"], json!("chat"));
}

#[test]
fn prompt_overrides_replace_the_defaults() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["a"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "s");

    let mut params = base_params();
    params["inquiry_prompt"] = json!("custom terms prompt");
    params["summary_prompt"] = json!("custom summary prompt");
    run(&host, params);

    let starts = host.calls_named(ACTION_START);
    assert_eq!(starts[0]["params"]["messages"][0]["content"], json!("custom terms prompt"));
    assert_eq!(starts[1]["params"]["messages"][0]["content"], json!("custom summary prompt"));
}

#[test]
fn connection_overrides_are_forwarded_to_every_llm_call() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["a"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "s");

    let mut params = base_params();
    params["base_url"] = json!("http://box:9999");
    params["timeout_secs"] = json!(30);
    params["options"] = json!({ "num_ctx": 8192 });
    run(&host, params);

    for start in host.calls_named(ACTION_START) {
        assert_eq!(start["params"]["base_url"], json!("http://box:9999"));
        assert_eq!(start["params"]["timeout_secs"], json!(30));
        // Added, not substituted for: both phases already set their own
        // temperature, and raising num_ctx must not cost the caller that.
        assert_eq!(start["params"]["options"]["num_ctx"], json!(8192));
        assert_eq!(start["params"]["options"]["temperature"], json!(0));
    }
}

// ── error propagation ────────────────────────────────────────────────────────

#[test]
fn llm_call_failure_on_terms_phase_is_llm_error() {
    let host = FakeHost::new();
    host.push_ok(ACTION_START, json!({ "invocation_id": "inv-1", "action_ref": LLM_REF, "console_seq_start": 0 }));
    host.push_ok(ACTION_CANCELLED, json!({ "cancelled": false }));
    host.push_ok(
        ACTION_POLL,
        json!({
            "invocation_id": "inv-1", "action_ref": LLM_REF, "status": "failed",
            "result": Value::Null, "error": "model not found",
        }),
    );
    host.push_ok(CONSOLE_TAIL, json!({ "entries": [], "next_cursor": 0, "first_seq": 0, "dropped": 0 }));
    host.push_ok(CONSOLE_COPY, json!({ "copied": 0, "next_cursor": 0 }));

    let out = run(&host, base_params());

    assert!(!out.success);
    assert_eq!(kind(&out), "llm_error");
    assert_eq!(out.output["stage"], json!("terms"));
    assert!(out.message.unwrap().contains("model not found"));
}

#[test]
fn search_failure_is_reported_with_the_offending_term() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["auth"]}"#);
    host.push_fail(DOCUMENT_SEARCH_REF, "fts index unavailable");

    let out = run(&host, base_params());

    assert!(!out.success);
    assert_eq!(kind(&out), "search_error");
    assert_eq!(out.output["term"], json!("auth"));
}

#[test]
fn llm_call_failure_on_summary_phase_is_llm_error() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["auth"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_ok(ACTION_START, json!({ "invocation_id": "inv-2", "action_ref": LLM_REF, "console_seq_start": 0 }));
    host.push_ok(ACTION_CANCELLED, json!({ "cancelled": false }));
    host.push_ok(
        ACTION_POLL,
        json!({
            "invocation_id": "inv-2", "action_ref": LLM_REF, "status": "failed",
            "result": Value::Null, "error": "context length exceeded",
        }),
    );
    host.push_ok(CONSOLE_TAIL, json!({ "entries": [], "next_cursor": 0, "first_seq": 0, "dropped": 0 }));
    host.push_ok(CONSOLE_COPY, json!({ "copied": 0, "next_cursor": 0 }));

    let out = run(&host, base_params());

    assert!(!out.success);
    assert_eq!(kind(&out), "llm_error");
    assert_eq!(out.output["stage"], json!("summary"));
}

#[test]
fn unknown_and_absent_fn_names() {
    let host = FakeHost::new();
    let out = solx_inquiry::dispatch(&host, Some("nope"), "{}");
    assert!(!out.success);
    assert_eq!(kind(&out), "unknown_action");
    assert_eq!(out.output["fn_name"], json!("nope"));
    assert_eq!(out.output["known"], json!(["inquire", "instruct"]));

    let out = solx_inquiry::dispatch(&host, None, "{}");
    assert_eq!(kind(&out), "unknown_action");
}

#[test]
fn malformed_params() {
    let host = FakeHost::new();

    let out = solx_inquiry::dispatch(&host, Some("inquire"), "not json");
    assert_eq!(kind(&out), "bad_params");

    let out = solx_inquiry::dispatch(&host, Some("inquire"), "[1,2,3]");
    assert_eq!(kind(&out), "bad_params");
}

// ── action relevance ordering ────────────────────────────────────────────────

#[test]
fn per_term_search_limit_stays_wide_even_when_max_results_is_small() {
    // A small max_results must only cap the *final* merged list, not each
    // term's own candidate fetch - otherwise the actual best match across
    // every term could be excluded before merging ever saw it.
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "s");

    let mut params = base_params();
    params["max_results"] = json!(2);
    run(&host, params);

    let search_call = host.calls_named(DOCUMENT_SEARCH_REF)[0].clone();
    assert!(search_call["limit"].as_u64().unwrap() >= 10, "{search_call}");
}

#[test]
fn action_hits_keep_search_actions_relevance_order() {
    // search_actions returns `items` already ordered by FTS5 rank (best
    // match first) - solx-inquiry has no numeric score to read back, only
    // that ordering, so it must survive into the final result rather than
    // being scrambled by an unordered merge structure.
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({
            "items": [
                { "id": "1", "path": "/packages/x", "name": "best-match", "caption": "Best" },
                { "id": "2", "path": "/packages/x", "name": "second-match", "caption": "Second" },
                { "id": "3", "path": "/packages/x", "name": "third-match", "caption": "Third" },
            ],
            "total": 3, "limit": 10, "offset": 0,
        }),
    );
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let params = json!({ "inquiry": "how do I deploy?", "model": "m", "scope": "actions" });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    let names: Vec<&str> = out.output["hits"].as_array().unwrap().iter().map(|h| h["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["best-match", "second-match", "third-match"]);
    let scores: Vec<f64> = out.output["hits"].as_array().unwrap().iter().map(|h| h["score"].as_f64().unwrap()).collect();
    assert!(scores[0] > scores[1] && scores[1] > scores[2], "{scores:?}");
}

#[test]
fn an_action_found_by_multiple_terms_accumulates_their_ranks() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy", "release"]}"#);
    // Under "deploy" it ranks second; under "release" it ranks first.
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({
            "items": [
                { "id": "1", "path": "/packages/x", "name": "other", "caption": "Other" },
                { "id": "2", "path": "/packages/x", "name": "deploy", "caption": "Deploy" },
            ],
            "total": 2, "limit": 10, "offset": 0,
        }),
    );
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({
            "items": [{ "id": "2", "path": "/packages/x", "name": "deploy", "caption": "Deploy" }],
            "total": 1, "limit": 10, "offset": 0,
        }),
    );
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let params = json!({ "inquiry": "how do I deploy or release?", "model": "m", "scope": "actions" });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    let hits = out.output["hits"].as_array().unwrap();
    let deploy_hit = hits.iter().find(|h| h["name"] == "deploy").unwrap();
    // Rank fusion: second place under "deploy" (0.5) *plus* first place under
    // "release" (1.0). Taking the best of the two instead would score it 1.0
    // and tie it with "other", which only ever matched one term - corroboration
    // is what the sum is for.
    assert_eq!(deploy_hit["score"], json!(1.5));
    let other_hit = hits.iter().find(|h| h["name"] == "other").unwrap();
    assert_eq!(other_hit["score"], json!(1.0));
    assert_eq!(hits[0]["name"], json!("deploy"), "the corroborated hit ranks first: {hits:?}");
}

#[test]
fn a_tie_on_score_breaks_toward_the_hit_matched_by_more_terms() {
    // Three different actions can each legitimately rank #1 under their own
    // single term (every action score is a reciprocal rank), tying at 1.0.
    // The one corroborated by more of the model's terms is stronger
    // evidence and must win the tie, rather than falling back to whatever
    // order the merge map's keys happen to sort in.
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["alpha", "beta", "gamma"]}"#);
    // "widely-matched" ranks #1 under "alpha" and "beta"; "one-hit-wonder"
    // ranks #1 under "gamma" only. Both tie at score 1.0 after merging.
    host.push_ok(ACTION_SEARCH_REF, json!({ "items": [{ "id": "1", "path": "/packages/z", "name": "widely-matched", "caption": "W" }], "total": 1, "limit": 10, "offset": 0 }));
    host.push_ok(ACTION_SEARCH_REF, json!({ "items": [{ "id": "1", "path": "/packages/z", "name": "widely-matched", "caption": "W" }], "total": 1, "limit": 10, "offset": 0 }));
    host.push_ok(ACTION_SEARCH_REF, json!({ "items": [{ "id": "2", "path": "/packages/a", "name": "one-hit-wonder", "caption": "O" }], "total": 1, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let params = json!({ "inquiry": "q", "model": "m", "scope": "actions", "max_results": 1 });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    let hits = out.output["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["name"], json!("widely-matched"), "{hits:?}");
}

// ── scope / merge (unaffected by the detached-call plumbing) ───────────────

#[test]
fn scope_both_searches_documents_and_actions_and_tags_source() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({ "items": [{ "id": "1", "path": "/notes", "name": "deploy-notes", "typeRef": "x", "contents": { "body": "run the deploy script" } }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({ "items": [{ "id": "2", "path": "/packages/x", "name": "deploy", "caption": "Deploy", "description": "Deploys the thing", "category": "ops", "paramTypeRef": "/packages/x/DeployParams" }], "total": 1, "limit": 10, "offset": 0 }),
    );
    // The document hit's contents come back inline from search_documents;
    // the action hit triggers entity_get_type for its paramTypeRef's schema
    // - its caption/description/category were already in hand from
    // search_actions.
    host.push_ok(TYPE_GET_REF, json!({ "schema": { "type": "object", "required": ["target"] } }));
    host.push_llm_call_detached_quiet("inv-summary", "Use /packages/x/deploy.");

    let params = json!({ "inquiry": "how do I deploy?", "model": "m", "scope": "both" });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    let hits = out.output["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    let sources: Vec<&str> = hits.iter().map(|h| h["source"].as_str().unwrap()).collect();
    assert!(sources.contains(&"document"));
    assert!(sources.contains(&"action"));

    let doc_hit = hits.iter().find(|h| h["source"] == "document").unwrap();
    assert_eq!(doc_hit["details"], json!({ "body": "run the deploy script" }));
    let action_hit = hits.iter().find(|h| h["source"] == "action").unwrap();
    assert_eq!(
        action_hit["details"],
        json!({ "category": "ops", "paramTypeRef": "/packages/x/DeployParams", "paramSchema": { "type": "object", "required": ["target"] } })
    );
    assert!(!host.call_names().contains(&"/builtin/document/entity_get_document".to_string()));
    let type_calls = host.calls_named(TYPE_GET_REF);
    assert_eq!(type_calls.len(), 1);
    assert_eq!(type_calls[0], json!({ "path": "/packages/x", "name": "DeployParams" }));
}

#[test]
fn two_actions_sharing_a_param_type_fetch_its_schema_once() {
    // `TypeCache` (see `search::TypeCache`) caches a type by reference for the
    // life of one `run_search_with` call. Two distinct actions - not the same
    // hit found by two terms, which `merge` already dedupes - sharing a
    // `paramTypeRef` used to fetch it twice; this call should fetch it once
    // and apply the same schema to both.
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({
            "items": [
                { "id": "1", "path": "/packages/x", "name": "deploy", "caption": "Deploy", "paramTypeRef": "/packages/x/TargetParams" },
                { "id": "2", "path": "/packages/x", "name": "redeploy", "caption": "Redeploy", "paramTypeRef": "/packages/x/TargetParams" },
            ],
            "total": 2, "limit": 10, "offset": 0,
        }),
    );
    host.push_ok(TYPE_GET_REF, json!({ "schema": { "type": "object", "required": ["target"] } }));
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let params = json!({ "inquiry": "how do I deploy?", "model": "m", "scope": "actions" });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    let type_calls = host.calls_named(TYPE_GET_REF);
    assert_eq!(type_calls.len(), 1, "{type_calls:?}");
    let hits = out.output["hits"].as_array().unwrap();
    for hit in hits {
        assert_eq!(
            hit["details"]["paramSchema"],
            json!({ "type": "object", "required": ["target"] }),
            "{hits:?}"
        );
    }
}

#[test]
fn action_param_schema_fetch_is_best_effort() {
    // Same contract as document enrichment: a dead/failing entity_get_type
    // must not sink the inquiry - the action hit just keeps whatever it
    // already had (category/phrases/paramTypeRef), without paramSchema.
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({ "items": [{ "id": "1", "path": "/packages/x", "name": "deploy", "caption": "Deploy", "paramTypeRef": "/packages/x/DeployParams" }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_fail(TYPE_GET_REF, "type was deleted");
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let params = json!({ "inquiry": "how do I deploy?", "model": "m", "scope": "actions" });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["hits"][0]["details"], json!({ "paramTypeRef": "/packages/x/DeployParams" }));
}

#[test]
fn action_hit_without_param_type_ref_makes_no_type_call() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({ "items": [{ "id": "1", "path": "/packages/x", "name": "deploy", "caption": "Deploy" }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let params = json!({ "inquiry": "how do I deploy?", "model": "m", "scope": "actions" });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    assert!(host.calls_named(TYPE_GET_REF).is_empty());
}

#[test]
fn action_hit_with_both_type_refs_fetches_both_schemas() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({ "items": [{ "id": "1", "path": "/packages/x", "name": "deploy", "caption": "Deploy", "paramTypeRef": "/packages/x/DeployParams", "resultTypeRef": "/packages/x/DeployResult" }], "total": 1, "limit": 10, "offset": 0 }),
    );
    // Popped in call order: paramTypeRef fetched before resultTypeRef.
    host.push_ok(TYPE_GET_REF, json!({ "schema": { "type": "object", "required": ["target"] } }));
    host.push_ok(TYPE_GET_REF, json!({ "schema": { "type": "object", "properties": { "ok": { "type": "boolean" } } } }));
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let params = json!({ "inquiry": "how do I deploy?", "model": "m", "scope": "actions" });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    assert_eq!(
        out.output["hits"][0]["details"],
        json!({
            "paramTypeRef": "/packages/x/DeployParams",
            "resultTypeRef": "/packages/x/DeployResult",
            "paramSchema": { "type": "object", "required": ["target"] },
            "resultSchema": { "type": "object", "properties": { "ok": { "type": "boolean" } } },
        })
    );

    let type_calls = host.calls_named(TYPE_GET_REF);
    assert_eq!(type_calls.len(), 2);
    assert_eq!(type_calls[0], json!({ "path": "/packages/x", "name": "DeployParams" }));
    assert_eq!(type_calls[1], json!({ "path": "/packages/x", "name": "DeployResult" }));
}

#[test]
fn result_schema_fetch_failure_does_not_affect_param_schema() {
    // Independent, best-effort per ref: a failed resultTypeRef fetch must
    // not take paramSchema down with it (or vice versa).
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({ "items": [{ "id": "1", "path": "/packages/x", "name": "deploy", "caption": "Deploy", "paramTypeRef": "/packages/x/DeployParams", "resultTypeRef": "/packages/x/DeployResult" }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_ok(TYPE_GET_REF, json!({ "schema": { "type": "object" } }));
    host.push_fail(TYPE_GET_REF, "type was deleted");
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let params = json!({ "inquiry": "how do I deploy?", "model": "m", "scope": "actions" });
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    let details = &out.output["hits"][0]["details"];
    assert_eq!(details["paramSchema"], json!({ "type": "object" }));
    assert!(details.get("resultSchema").is_none());
}

#[test]
fn duplicate_hits_across_terms_are_merged_and_their_ranks_fused() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["auth", "login"]}"#);
    // Under "auth" it ranks second (0.5, behind "other"); under "login" it
    // ranks first (1.0). The merge must fold the two into one row whose score
    // is the sum, rather than keeping one of them and discarding the evidence
    // the other represents.
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({
            "items": [
                { "id": "0", "path": "/notes", "name": "other", "typeRef": "x" },
                { "id": "1", "path": "/notes", "name": "auth", "typeRef": "x" },
            ],
            "total": 2, "limit": 10, "offset": 0,
        }),
    );
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({ "items": [{ "id": "1", "path": "/notes", "name": "auth", "typeRef": "x" }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    let hits = out.output["hits"].as_array().unwrap();
    assert_eq!(
        hits.iter().filter(|h| h["name"] == "auth").count(),
        1,
        "the same doc hit by two terms must merge into one, not appear twice: {hits:?}"
    );
    let auth_hit = hits.iter().find(|h| h["name"] == "auth").unwrap();
    assert_eq!(auth_hit["score"], json!(1.5));
    let matched: Vec<&str> = auth_hit["matched_terms"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(matched, vec!["auth", "login"]);
}

#[test]
fn no_hits_still_produces_a_summary_without_inventing_one() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["nonexistent-topic"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "Nothing was found for this inquiry.");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["hits"], json!([]));
    assert_eq!(out.output["summary"], json!("Nothing was found for this inquiry."));

    let starts = host.calls_named(ACTION_START);
    let user_msg = starts[1]["params"]["messages"][1]["content"].as_str().unwrap();
    assert!(user_msg.contains("No matching documents or actions were found"));
}

#[test]
fn hits_are_capped_at_max_results_after_merging() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["a"]}"#);
    // Best FTS5 rank first (position 0), same convention as search_actions.
    let items: Vec<Value> = (0..5)
        .map(|i| json!({ "id": i.to_string(), "path": "/p", "name": format!("n{i}"), "typeRef": "x" }))
        .collect();
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": items, "total": 5, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "s");

    let mut params = base_params();
    params["max_results"] = json!(2);
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    let out_hits = out.output["hits"].as_array().unwrap();
    assert_eq!(out_hits.len(), 2);
    // The two best-ranked (lowest-index) items survive the cap.
    assert_eq!(out_hits[0]["name"], json!("n0"));
    assert_eq!(out_hits[1]["name"], json!("n1"));
    assert!(out_hits[0]["score"].as_f64().unwrap() > out_hits[1]["score"].as_f64().unwrap());
}

#[test]
fn document_hits_keep_search_documents_relevance_order() {
    // search_documents now returns `items` already ordered by FTS5 rank
    // (best match first), mirroring search_actions - same treatment, same
    // guarantee, same test.
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["deploy"]}"#);
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({
            "items": [
                { "id": "1", "path": "/notes", "name": "best-match", "typeRef": "x" },
                { "id": "2", "path": "/notes", "name": "second-match", "typeRef": "x" },
                { "id": "3", "path": "/notes", "name": "third-match", "typeRef": "x" },
            ],
            "total": 3, "limit": 10, "offset": 0,
        }),
    );
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    let names: Vec<&str> = out.output["hits"].as_array().unwrap().iter().map(|h| h["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["best-match", "second-match", "third-match"]);
    let scores: Vec<f64> = out.output["hits"].as_array().unwrap().iter().map(|h| h["score"].as_f64().unwrap()).collect();
    assert!(scores[0] > scores[1] && scores[1] > scores[2], "{scores:?}");
}

// ── vendored wit ─────────────────────────────────────────────────────────────

#[test]
fn vendored_wit_matches_solx_core_when_present() {
    // solx-packages is a separate repo, so the sibling checkout may not exist;
    // this only guards a dev machine that has both.
    let sibling = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../solx-core/solx-wasm/wit/custom-action.wit");
    let Ok(upstream) = std::fs::read_to_string(&sibling) else {
        return;
    };
    assert_eq!(
        upstream.replace("\r\n", "\n"),
        include_str!("../wit/custom-action.wit").replace("\r\n", "\n"),
        "vendored wit/custom-action.wit has drifted from solx-core"
    );
}

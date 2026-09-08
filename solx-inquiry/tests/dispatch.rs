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
use solx_inquiry::search::{ACTION_SEARCH_REF, DOCUMENT_GET_REF, DOCUMENT_SEARCH_REF, TYPE_GET_REF};

const LLM_REF: &str = "/packages/solx-ollama/ollama-chat";
const ACTION_START: &str = "/builtin/action/start";
const ACTION_POLL: &str = "/builtin/action/poll";
const ACTION_STOP: &str = "/builtin/action/stop";
const ACTION_CANCELLED: &str = "/builtin/action/cancelled";
const CONSOLE_TAIL: &str = "/builtin/console/tail";
const CONSOLE_PRINT: &str = "/builtin/console/print";

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
    /// (`ok`, carrying `content`), then `console/tail` (+ one
    /// `console/print` per entry tagged with this call's own
    /// `invocation_id` — the shared-console filter in `src/llm.rs` only
    /// echoes those).
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

    /// One `console/tail` response, plus one `console/print` response per
    /// entry tagged with `invocation_id_filter` — matching exactly how many
    /// `console/print` calls `src/llm.rs`'s echo filter will actually make.
    fn push_tail_with_entries(&self, invocation_id_filter: &str, entries: Vec<Value>) -> &Self {
        let matching = entries
            .iter()
            .filter(|e| e.get("invocation_id").and_then(Value::as_str) == Some(invocation_id_filter))
            .count();
        let next_cursor = entries.len() as i64;
        self.push_ok(CONSOLE_TAIL, json!({ "entries": entries, "next_cursor": next_cursor, "first_seq": 0, "dropped": 0 }));
        for _ in 0..matching {
            self.push_ok(CONSOLE_PRINT, json!({ "seq": 1 }));
        }
        self
    }

    /// Queue one `entity_get_document` response carrying `contents` — one
    /// call per document-source hit that survives capping, in score-descending
    /// order (the order `enrich_documents` walks the final hit list).
    fn push_doc_contents(&self, contents: Value) -> &Self {
        self.push_ok(DOCUMENT_GET_REF, json!({ "contents": contents }))
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
            "hits": [{ "id": "1", "path": "/notes", "name": "auth", "title": "Auth notes", "summary": "how login works", "typeRef": "/types/core/Object", "score": 4.2 }],
            "total": 1, "limit": 10, "offset": 0,
        }),
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_doc_contents(json!({ "body": "session tokens are issued at login" }));
    host.push_llm_call_detached_quiet("inv-summary", "Authentication uses session tokens, per /notes/auth.");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["terms"], json!(["authentication", "session token"]));
    assert_eq!(out.output["hits"][0]["path"], json!("/notes"));
    assert_eq!(out.output["hits"][0]["details"], json!({ "body": "session tokens are issued at login" }));
    assert_eq!(out.output["summary"], json!("Authentication uses session tokens, per /notes/auth."));

    assert_eq!(host.calls_named(ACTION_START).len(), 2, "one detached start per llm phase");
    assert_eq!(host.calls_named(DOCUMENT_SEARCH_REF).len(), 2);
    assert!(host.calls_named(ACTION_SEARCH_REF).is_empty());
}

#[test]
fn console_output_from_the_detached_call_is_echoed_into_inquires_own_console() {
    let host = FakeHost::new();
    host.push_llm_call_detached(
        "inv-terms",
        r#"{"terms": ["auth"]}"#,
        vec![console_entry("inv-terms", "thinking about it")],
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "no results");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    let prints = host.calls_named(CONSOLE_PRINT);
    assert_eq!(prints.len(), 1, "{prints:?}");
    assert_eq!(prints[0]["message"], json!("[terms] thinking about it"));
    assert_eq!(prints[0]["level"], json!("chunk"));
}

#[test]
fn console_entries_from_a_different_invocation_are_not_echoed() {
    // The child action's console is shared across every concurrent caller
    // (keyed by action_ref alone) — a line from some other invocation must
    // never leak into this inquiry's own console.
    let host = FakeHost::new();
    host.push_llm_call_detached(
        "inv-terms",
        r#"{"terms": ["auth"]}"#,
        vec![
            console_entry("some-other-concurrent-call", "not mine"),
            console_entry("inv-terms", "mine"),
        ],
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "no results");

    run(&host, base_params());

    let prints = host.calls_named(CONSOLE_PRINT);
    assert_eq!(prints.len(), 1);
    assert_eq!(prints[0]["message"], json!("[terms] mine"));
}

#[test]
fn a_still_running_poll_loops_and_keeps_draining_console_until_terminal() {
    let host = FakeHost::new();
    host.push_llm_call_detached_two_polls(
        "inv-terms",
        vec![console_entry("inv-terms", "still working")],
        r#"{"terms": ["auth"]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "no results");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["terms"], json!(["auth"]));
    assert_eq!(host.calls_named(ACTION_POLL).len(), 3, "2 for terms + 1 for summary");
    let prints = host.calls_named(CONSOLE_PRINT);
    assert_eq!(prints.len(), 1);
    assert_eq!(prints[0]["message"], json!("[terms] still working"));
}

// ── fallback (non-long-lived host) ──────────────────────────────────────────

#[test]
fn falls_back_to_a_blocking_call_when_the_host_cannot_detach() {
    let host = FakeHost::new();
    host.push_llm_call_fallback(r#"{"terms": ["auth"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
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
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
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
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
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
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
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
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
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
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_llm_call_detached_quiet("inv-summary", "s");

    let mut params = base_params();
    params["base_url"] = json!("http://box:9999");
    params["timeout_secs"] = json!(30);
    run(&host, params);

    for start in host.calls_named(ACTION_START) {
        assert_eq!(start["params"]["base_url"], json!("http://box:9999"));
        assert_eq!(start["params"]["timeout_secs"], json!(30));
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
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
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
    assert_eq!(out.output["known"], json!(["inquire"]));

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
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
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
fn an_action_found_by_multiple_terms_keeps_its_best_rank() {
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
    // Best-of-both: first place under "release" (score 1.0), not second
    // place under "deploy" (score 0.5).
    assert_eq!(deploy_hit["score"], json!(1.0));
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
        json!({ "hits": [{ "id": "1", "path": "/notes", "name": "deploy-notes", "typeRef": "x", "score": 2.0 }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_ok(
        ACTION_SEARCH_REF,
        json!({ "items": [{ "id": "2", "path": "/packages/x", "name": "deploy", "caption": "Deploy", "description": "Deploys the thing", "category": "ops", "paramTypeRef": "/packages/x/DeployParams" }], "total": 1, "limit": 10, "offset": 0 }),
    );
    // The document hit triggers entity_get_document (contents); the action
    // hit triggers entity_get_type for its paramTypeRef's schema, not
    // entity_get_document - its caption/description/category were already
    // in hand from search_actions.
    host.push_doc_contents(json!({ "body": "run the deploy script" }));
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
    assert_eq!(host.calls_named(DOCUMENT_GET_REF).len(), 1, "no entity_get_document call for the action hit");
    let type_calls = host.calls_named(TYPE_GET_REF);
    assert_eq!(type_calls.len(), 1);
    assert_eq!(type_calls[0], json!({ "path": "/packages/x", "name": "DeployParams" }));
}

#[test]
fn document_content_enrichment_failure_is_best_effort() {
    // A dead/failing entity_get_document must not sink the whole inquiry
    // over what is strictly additional context — the hit just keeps
    // details: null, same as if it were never fetched.
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["auth"]}"#);
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({ "hits": [{ "id": "1", "path": "/notes", "name": "auth", "typeRef": "x", "score": 1.0 }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_fail(DOCUMENT_GET_REF, "document was deleted");
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["hits"][0]["details"], Value::Null);
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
fn duplicate_hits_across_terms_are_merged_keeping_the_best_score() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["auth", "login"]}"#);
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({ "hits": [{ "id": "1", "path": "/notes", "name": "auth", "typeRef": "x", "score": 1.0 }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({ "hits": [{ "id": "1", "path": "/notes", "name": "auth", "typeRef": "x", "score": 3.5 }], "total": 1, "limit": 10, "offset": 0 }),
    );
    host.push_doc_contents(json!({ "body": "auth details" }));
    host.push_llm_call_detached_quiet("inv-summary", "summary");

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    let hits = out.output["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "the same doc hit by two terms must merge into one");
    assert_eq!(hits[0]["score"], json!(3.5));
    let matched: Vec<&str> = hits[0]["matched_terms"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(matched, vec!["auth", "login"]);
}

#[test]
fn no_hits_still_produces_a_summary_without_inventing_one() {
    let host = FakeHost::new();
    host.push_llm_call_detached_quiet("inv-terms", r#"{"terms": ["nonexistent-topic"]}"#);
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": [], "total": 0, "limit": 10, "offset": 0 }));
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
    let hits: Vec<Value> = (0..5)
        .map(|i| json!({ "id": i.to_string(), "path": "/p", "name": format!("n{i}"), "typeRef": "x", "score": i as f64 }))
        .collect();
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "hits": hits, "total": 5, "limit": 10, "offset": 0 }));
    // Only the 2 hits that survive capping get enriched, not all 5 raw ones.
    host.push_doc_contents(json!({ "n": 4 }));
    host.push_doc_contents(json!({ "n": 3 }));
    host.push_llm_call_detached_quiet("inv-summary", "s");

    let mut params = base_params();
    params["max_results"] = json!(2);
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    let out_hits = out.output["hits"].as_array().unwrap();
    assert_eq!(out_hits.len(), 2);
    assert_eq!(out_hits[0]["score"], json!(4.0));
    assert_eq!(out_hits[1]["score"], json!(3.0));
    assert_eq!(host.calls_named(DOCUMENT_GET_REF).len(), 2, "must not enrich hits dropped by the cap");
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

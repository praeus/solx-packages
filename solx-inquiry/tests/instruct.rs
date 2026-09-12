//! Host-target tests for the whole `instruct` pipeline.
//!
//! The `FakeHost` here is a sibling of the one in `dispatch.rs`, not a copy of
//! it, because `instruct` needs something that one cannot do: **script several
//! concurrent children independently**. A fan-out polls three invocations that
//! are all the same `action_ref`, so a single FIFO queue per ref could not say
//! which child a given poll response belonged to — the queue order would
//! decide, and the order polls arrive in is exactly what these tests exist to
//! pin down. So `action/poll` is keyed by `invocation_id` instead.
//!
//! Three refs answer from a default when nothing is queued, rather than
//! panicking: `console/print` (fire-and-forget observability, called a dozen
//! times a run), `console/tail` (nothing to drain) and `action/cancelled`
//! (not cancelled). Queued responses still take precedence, which is what lets
//! the cancellation test say "not cancelled, then cancelled" precisely.
//! Everything else still panics when it runs dry, so an unexpected call is
//! loud rather than silently absorbed.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use serde_json::{json, Value};

use solx_inquiry::host::{Host, HostCall, Outcome};
use solx_inquiry::recall::DOCUMENT_LIST_REF;
use solx_inquiry::search::{ACTION_SEARCH_REF, DOCUMENT_SEARCH_REF, TYPE_GET_REF};
use solx_inquiry::session::{DOCUMENT_GET_REF, DOCUMENT_SAVE_REF};

const LLM_REF: &str = "/packages/solx-ollama/ollama-chat";
const ACTION_START: &str = "/builtin/action/start";
const ACTION_POLL: &str = "/builtin/action/poll";
const ACTION_STOP: &str = "/builtin/action/stop";
const ACTION_CANCELLED: &str = "/builtin/action/cancelled";
const CONSOLE_TAIL: &str = "/builtin/console/tail";
const CONSOLE_PRINT: &str = "/builtin/console/print";
const CONSOLE_COPY: &str = "/builtin/console/copy";

const SESSION: &str = "/solx-inquiry/sessions/test";
/// Optional, and never defaulted. Supplying it is the caller declaring where
/// its own past output lives, which is what lets that output be kept out of a
/// later inquiry's evidence; omitting it turns memories off entirely.
const MEMORY_PATH: &str = "/solx-inquiry/memories";
const INSTRUCT_AUTHOR: &str = "/packages/solx-inquiry/instruct";

struct FakeHost {
    calls: RefCell<Vec<(String, Value)>>,
    responses: RefCell<HashMap<String, VecDeque<Result<HostCall, String>>>>,
    /// `action/poll` responses keyed by invocation id, so three concurrent
    /// children can be driven independently of the order they are polled in.
    polls: RefCell<HashMap<String, VecDeque<Value>>>,
}

impl FakeHost {
    fn new() -> Self {
        FakeHost {
            calls: RefCell::new(Vec::new()),
            responses: RefCell::new(HashMap::new()),
            polls: RefCell::new(HashMap::new()),
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

    /// Queue one `action/start` success. Starts are consumed in call order:
    /// the intent call first, then one per inquiry.
    fn push_start(&self, invocation_id: &str) -> &Self {
        self.push_ok(
            ACTION_START,
            json!({ "invocation_id": invocation_id, "action_ref": LLM_REF, "console_seq_start": 0 }),
        )
    }

    fn push_poll(&self, invocation_id: &str, poll: Value) -> &Self {
        self.polls
            .borrow_mut()
            .entry(invocation_id.to_string())
            .or_default()
            .push_back(poll);
        self
    }

    /// A child that completes with `content` the first time it is polled.
    fn push_done(&self, invocation_id: &str, content: &str) -> &Self {
        self.push_poll(
            invocation_id,
            json!({
                "invocation_id": invocation_id, "action_ref": LLM_REF, "status": "ok",
                "result": { "message": { "role": "assistant", "content": content } },
                "error": Value::Null,
            }),
        )
    }

    /// A child that reports `running` once, then completes.
    fn push_running_then_done(&self, invocation_id: &str, content: &str) -> &Self {
        self.push_poll(
            invocation_id,
            json!({ "invocation_id": invocation_id, "status": "running", "result": Value::Null, "error": Value::Null }),
        );
        self.push_done(invocation_id, content)
    }

    fn push_failed(&self, invocation_id: &str, error: &str) -> &Self {
        self.push_poll(
            invocation_id,
            json!({
                "invocation_id": invocation_id, "action_ref": LLM_REF, "status": "failed",
                "result": Value::Null, "error": error,
            }),
        )
    }

    /// One blocking llm response, for the no-long-lived-host path.
    fn push_blocking(&self, content: &str) -> &Self {
        self.push_ok(LLM_REF, json!({ "message": { "role": "assistant", "content": content } }))
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

    fn count(&self, name: &str) -> usize {
        self.calls_named(name).len()
    }

    /// Index of the first call to `name`, for asserting ordering.
    fn first_index(&self, name: &str) -> Option<usize> {
        self.call_names().iter().position(|n| n == name)
    }
}

impl Host for FakeHost {
    fn exec(&self, action_ref: &str, payload: &Value) -> Result<HostCall, String> {
        self.calls.borrow_mut().push((action_ref.to_string(), payload.clone()));

        if action_ref == ACTION_POLL {
            if let Some(id) = payload.get("invocation_id").and_then(Value::as_str) {
                if let Some(queue) = self.polls.borrow_mut().get_mut(id) {
                    // The last queued response repeats, so a spin-guard poll
                    // (which is an extra poll of an already-answered child)
                    // does not exhaust the script.
                    let value = if queue.len() > 1 { queue.pop_front() } else { queue.front().cloned() };
                    if let Some(value) = value {
                        return Ok(HostCall { success: true, message: None, result: value });
                    }
                }
            }
        }

        let queued = self
            .responses
            .borrow_mut()
            .get_mut(action_ref)
            .and_then(VecDeque::pop_front);
        if let Some(queued) = queued {
            return queued;
        }

        let default = match action_ref {
            CONSOLE_PRINT => json!({ "seq": 1 }),
            CONSOLE_TAIL => json!({ "entries": [], "next_cursor": 0, "first_seq": 0, "dropped": 0 }),
            // `drain_console` calls this once per tracked invocation on every
            // drain, unconditionally — cheap when there is nothing new (see
            // solx-console's own tests), which this default mirrors: nothing
            // copied, and the cursor holds rather than advancing.
            CONSOLE_COPY => json!({
                "copied": 0,
                "next_cursor": payload.get("cursor").and_then(Value::as_i64).unwrap_or(0),
            }),
            ACTION_CANCELLED => json!({ "cancelled": false }),
            _ => panic!("FakeHost ran out of responses at {action_ref}"),
        };
        Ok(HostCall { success: true, message: None, result: default })
    }

    fn log(&self, _msg: &str) {}
}

fn run(host: &FakeHost, params: Value) -> Outcome {
    solx_inquiry::dispatch(host, Some("instruct"), &params.to_string())
}

fn base_params() -> Value {
    json!({
        "instruction": "how does auth work, and how do I search for it?",
        "model": "qwen3:4b",
        "session": SESSION,
        "memory_path": MEMORY_PATH,
    })
}

fn kind(outcome: &Outcome) -> String {
    outcome.output.get("kind").and_then(Value::as_str).unwrap_or("<none>").to_string()
}

/// Recall (skills, then memories) and the session read, all empty. Every test
/// that gets as far as the intent call needs these three.
fn push_empty_context(host: &FakeHost) {
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 40, "offset": 0 }));
    host.push_ok(DOCUMENT_LIST_REF, json!({ "items": [], "total": 0, "limit": 5, "offset": 0 }));
    host.push_fail(DOCUMENT_GET_REF, "not found");
}

fn doc_hits(items: Value) -> Value {
    json!({ "items": items, "total": 1, "limit": 10, "offset": 0 })
}

fn console_entry(invocation_id: &str, message: &str) -> Value {
    json!({ "seq": 1, "ts": "2026-01-01T00:00:00Z", "level": "chunk", "invocation_id": invocation_id,
            "run_id": Value::Null, "source": "guest", "message": message, "data": Value::Null })
}

// ── direct mode ─────────────────────────────────────────────────────────────

#[test]
fn a_direct_intent_answers_without_searching_or_fanning_out() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"Auth uses session tokens.","memory":true}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["intent"]["mode"], json!("direct"));
    assert_eq!(out.output["responses"][0]["text"], json!("Auth uses session tokens."));
    assert_eq!(out.output["scripts"], json!([]));
    // Exactly one llm call, and no per-term search at all.
    assert_eq!(host.count(ACTION_START), 1);
    assert_eq!(host.count(DOCUMENT_SEARCH_REF), 1, "recall only: skills");
    assert_eq!(host.count(DOCUMENT_LIST_REF), 1, "recall only: memories");
    assert!(host.calls_named(ACTION_SEARCH_REF).is_empty());
}

#[test]
fn a_direct_response_flagged_as_memory_is_reported_but_never_minted() {
    // The intent phase sees skills, memories and history - and no search
    // results at all. A direct answer is therefore model prior: it cites
    // nothing and nothing checked it. Minting one would write it down and feed
    // it back to a later run looking exactly like a grounded finding, which is
    // the loop `is_reference_material` closes from the other side.
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"Tokens expire hourly.","memory":true}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["memories"], json!([]), "a memory_path was given, and still nothing is minted");
    // The model's judgement still rides on the response, and the refusal is
    // said out loud rather than silently swallowing what it asked for.
    assert_eq!(out.output["responses"][0]["memory"], json!(true));
    let notes = out.output["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].as_str().unwrap().contains("ungrounded"), "{notes:?}");
}

#[test]
fn a_flagged_inquiry_response_comes_back_ready_to_save() {
    // The grounded half of the same rule: this one was produced by an inquiry,
    // against search results it cites, so it is mintable.
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"how long do tokens last?","terms":["tokens"]}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/notes", "name": "auth", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_done(
        "inv-0",
        r#"{"responses":[{"text":"Tokens expire hourly.","memory":true,"citations":["/notes/auth"]}]}"#,
    );
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    let memory = &out.output["memories"][0];
    assert_eq!(memory["path"], json!(MEMORY_PATH), "stamped with the path the caller declared");
    assert_eq!(memory["typeRef"], json!("/packages/solx-inquiry/InquiryMemory"));
    // Provenance: a saved memory is model output and should say so to whoever
    // reads it later. Nothing in the pipeline branches on this.
    assert_eq!(memory["author"], json!(INSTRUCT_AUTHOR));
    // The text is in `summary` as well as contents, which is what lets recall
    // be one lookup with no follow-up reads.
    assert_eq!(memory["summary"], json!("Tokens expire hourly."));
    assert_eq!(memory["contents"]["text"], json!("Tokens expire hourly."));
    assert!(memory["name"].as_str().unwrap().starts_with("tokens-expire-hourly-"));
}

#[test]
fn a_model_that_ignores_format_entirely_still_answers() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", "I already know: it uses session tokens.");
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["intent"]["mode"], json!("direct"));
    assert_eq!(out.output["responses"][0]["text"], json!("I already know: it uses session tokens."));
}

#[test]
fn a_next_prompt_reaches_the_outcome_and_the_saved_turn() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"direct","response":"created the file","next_prompt":"verify the file was created and report its size"}"#,
    );
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["next_prompt"], json!("verify the file was created and report its size"));
    assert_eq!(out.output["intent"]["next_prompt"], json!("verify the file was created and report its size"));

    let save = host.calls_named(DOCUMENT_SAVE_REF)[0].clone();
    let turns = save["contents"]["turns"].as_array().unwrap();
    assert_eq!(turns[0]["next_prompt"], json!("verify the file was created and report its size"));
}

#[test]
fn no_next_prompt_is_null_not_missing() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"Auth uses session tokens."}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["next_prompt"], Value::Null);
}

// ── the fan-out ─────────────────────────────────────────────────────────────

/// Intent proposing three inquiries: two document, one action.
const THREE_INQUIRIES: &str = r#"{"mode":"inquire","inquiries":[
    {"kind":"documents","question":"what is auth?","terms":["auth"]},
    {"kind":"documents","question":"what are sessions?","terms":["session"]},
    {"kind":"actions","question":"how do I search?","terms":["search"]}
]}"#;

fn push_three_inquiry_searches(host: &FakeHost) {
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/notes", "name": "auth", "typeRef": "x", "contents": { "body": "session tokens" } }])));
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "2", "path": "/notes", "name": "sessions", "typeRef": "x", "contents": { "body": "sessions expire" } }])));
    host.push_ok(ACTION_SEARCH_REF, doc_hits(json!([{ "id": "3", "path": "/builtin/document", "name": "search_documents", "caption": "Search" }])));
}

#[test]
fn every_inquiry_is_started_before_any_of_them_is_polled() {
    // This is the assertion that actually proves the inquiries run in
    // parallel rather than one after another. If the fan-out polled each
    // child to completion before starting the next, a poll would appear
    // between two starts.
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", THREE_INQUIRIES);
    push_three_inquiry_searches(&host);
    host.push_start("inv-0");
    host.push_start("inv-1");
    host.push_start("inv-2");
    host.push_done("inv-0", r#"{"responses":[{"text":"auth uses tokens"}]}"#);
    host.push_done("inv-1", r#"{"responses":[{"text":"sessions expire hourly"}]}"#);
    host.push_done("inv-2", r#"{"scripts":[{"steps":[{"action_ref":"/builtin/document/search_documents","params":{"q":"auth"}}]}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());
    assert!(out.success, "{:?}", out.message);

    let names = host.call_names();
    let starts: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| *n == ACTION_START)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(starts.len(), 4, "one intent call plus three inquiries");
    // The three inquiry starts must be contiguous with respect to polling:
    // no poll may fall between the first and the last of them.
    let fanout_starts = &starts[1..];
    let polls_between = names[fanout_starts[0]..*fanout_starts.last().unwrap()]
        .iter()
        .filter(|n| *n == ACTION_POLL)
        .count();
    assert_eq!(polls_between, 0, "a poll between two starts means they are not parallel");
}

#[test]
fn document_inquiries_produce_responses_and_an_action_inquiry_produces_a_script() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", THREE_INQUIRIES);
    push_three_inquiry_searches(&host);
    host.push_start("inv-0");
    host.push_start("inv-1");
    host.push_start("inv-2");
    host.push_done("inv-0", r#"{"responses":[{"text":"auth uses tokens","memory":true,"citations":["/notes/auth"]}]}"#);
    host.push_done("inv-1", r#"{"responses":[{"text":"sessions expire hourly"}]}"#);
    host.push_done(
        "inv-2",
        r#"{"scripts":[{"title":"Find auth","steps":[{"action_ref":"/builtin/document/search_documents","params":{"q":"auth"},"capture":"hits"}]}]}"#,
    );
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    let responses = out.output["responses"].as_array().unwrap();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["inquiry"], json!(0));
    assert_eq!(responses[1]["inquiry"], json!(1));
    assert_eq!(out.output["memories"].as_array().unwrap().len(), 1);

    let scripts = out.output["scripts"].as_array().unwrap();
    assert_eq!(scripts.len(), 1);
    assert_eq!(
        scripts[0]["source"],
        json!("$hits = exec /builtin/document/search_documents --json '{\"q\":\"auth\"}';\n")
    );
    assert_eq!(scripts[0]["actions"], json!(["/builtin/document/search_documents"]));
}

#[test]
fn a_child_that_reports_running_does_not_hold_up_the_others() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", THREE_INQUIRIES);
    push_three_inquiry_searches(&host);
    host.push_start("inv-0");
    host.push_start("inv-1");
    host.push_start("inv-2");
    host.push_running_then_done("inv-0", r#"{"responses":[{"text":"slow but done"}]}"#);
    host.push_done("inv-1", r#"{"responses":[{"text":"quick"}]}"#);
    host.push_done("inv-2", r#"{"scripts":[]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    let texts: Vec<&str> = out.output["responses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap())
        .collect();
    assert!(texts.contains(&"slow but done"));
    assert!(texts.contains(&"quick"));
    // inv-1 and inv-2 were terminal on their first poll, so only inv-0 is
    // polled a second time - the others are not re-polled once resolved.
    let polled: Vec<String> = host
        .calls_named(ACTION_POLL)
        .iter()
        .map(|p| p["invocation_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(polled.iter().filter(|id| *id == "inv-1").count(), 1);
    assert_eq!(polled.iter().filter(|id| *id == "inv-0").count(), 2);
}

#[test]
fn a_failing_console_tail_still_paces_the_fan_out() {
    // `drain_console` is best-effort - a console hiccup must not fail the run.
    // But the fan-out polls its children *without* `wait_secs` (one child must
    // never make another wait), which leaves the tail as the only thing in the
    // loop that can afford to sleep. A tail that errors returns instantly and
    // keeps doing so, so treating that like a quiet console would spin the
    // loop at full speed until the action timeout - hammering `action_poll`
    // for up to 8400s. It has to fall back to waiting on a child instead.
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"q","terms":["a"]}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/n", "name": "a", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_running_then_done("inv-0", r#"{"responses":[{"text":"eventually"}]}"#);
    // Two: the intent call drains one through its own echo before the fan-out
    // ever starts.
    host.push_fail(CONSOLE_TAIL, "console unavailable");
    host.push_fail(CONSOLE_TAIL, "console unavailable");
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["responses"][0]["text"], json!("eventually"));
    // The child was still running and the tail gave the loop nothing to wait
    // on, so the guard long-polled a child rather than going straight round
    // again.
    let waited = host
        .calls_named(ACTION_POLL)
        .iter()
        .any(|p| p["invocation_id"] == json!("inv-0") && p.get("wait_secs").is_some());
    assert!(waited, "{:?}", host.calls_named(ACTION_POLL));
}

#[test]
fn one_failed_inquiry_does_not_sink_the_others() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", THREE_INQUIRIES);
    push_three_inquiry_searches(&host);
    host.push_start("inv-0");
    host.push_start("inv-1");
    host.push_start("inv-2");
    host.push_failed("inv-0", "context length exceeded");
    host.push_done("inv-1", r#"{"responses":[{"text":"sessions expire hourly"}]}"#);
    host.push_done("inv-2", r#"{"scripts":[]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "a single failed inquiry must not fail the run: {:?}", out.message);
    assert_eq!(out.output["responses"].as_array().unwrap().len(), 1);
    let errors = out.output["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["inquiry"], json!(0));
    assert!(errors[0]["error"].as_str().unwrap().contains("context length exceeded"));
}

#[test]
fn every_inquiry_failing_is_a_failed_run_not_an_empty_answer() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"q","terms":["t"]}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 10, "offset": 0 }));
    host.push_start("inv-0");
    host.push_failed("inv-0", "model not found");

    let out = run(&host, base_params());

    assert!(!out.success);
    assert_eq!(kind(&out), "inquiry_error");
    assert_eq!(out.output["errors"].as_array().unwrap().len(), 1);
    // Nothing was produced, so nothing should have been written either.
    assert!(host.calls_named(DOCUMENT_SAVE_REF).is_empty());
}

// ── cancellation ────────────────────────────────────────────────────────────

#[test]
fn cancellation_stops_every_outstanding_child_and_returns_what_it_had() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    // Not cancelled during the intent call...
    host.push_ok(ACTION_CANCELLED, json!({ "cancelled": false }));
    host.push_done("inv-intent", THREE_INQUIRIES);
    push_three_inquiry_searches(&host);
    host.push_start("inv-0");
    host.push_start("inv-1");
    host.push_start("inv-2");
    // ...but cancelled on the fan-out's first check.
    host.push_ok(ACTION_CANCELLED, json!({ "cancelled": true }));
    host.push_ok(ACTION_STOP, json!({ "status": "cancelling" }));
    host.push_ok(ACTION_STOP, json!({ "status": "cancelling" }));
    host.push_ok(ACTION_STOP, json!({ "status": "cancelling" }));

    let out = run(&host, base_params());

    assert!(!out.success);
    assert_eq!(kind(&out), "cancelled");
    // Every child, not just the first: leaving two model calls running after
    // the pipeline was cancelled is exactly what this must not do.
    let stopped: Vec<String> = host
        .calls_named(ACTION_STOP)
        .iter()
        .map(|p| p["invocation_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(stopped, vec!["inv-0", "inv-1", "inv-2"]);
    // Cancellation is checked before polling, so a fan-out cancelled on its
    // first check never polls a child at all. (The intent call, which
    // completed before the cancellation, has its own poll.)
    let child_polls: Vec<Value> = host
        .calls_named(ACTION_POLL)
        .into_iter()
        .filter(|p| p["invocation_id"].as_str() != Some("inv-intent"))
        .collect();
    assert!(child_polls.is_empty(), "{child_polls:?}");
    assert_eq!(out.output["partial"]["instruction"], json!(base_params()["instruction"]));
}

// ── degraded host (no detached invocations) ─────────────────────────────────

#[test]
fn a_host_that_cannot_detach_runs_the_inquiries_sequentially_and_still_answers() {
    let host = FakeHost::new();
    push_empty_context(&host);
    // The intent call falls back first (llm::call's own fallback), then the
    // fan-out's start is refused too and it runs every job blocking.
    host.push_err(ACTION_START, "action_start requires a long-lived host (solx-server or solx-mcp).");
    host.push_blocking(THREE_INQUIRIES);
    push_three_inquiry_searches(&host);
    host.push_err(ACTION_START, "action_start requires a long-lived host (solx-server or solx-mcp).");
    host.push_blocking(r#"{"responses":[{"text":"auth uses tokens"}]}"#);
    host.push_blocking(r#"{"responses":[{"text":"sessions expire hourly"}]}"#);
    host.push_blocking(r#"{"scripts":[]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["responses"].as_array().unwrap().len(), 2);
    // Four blocking calls: intent plus three inquiries. Only one start was
    // attempted for the whole fan-out, not one per job.
    assert_eq!(host.count(LLM_REF), 4);
    assert_eq!(host.count(ACTION_START), 2);
    assert!(host.calls_named(ACTION_POLL).is_empty());
}

#[test]
fn a_start_failure_for_any_other_reason_fails_only_that_inquiry() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[
            {"kind":"documents","question":"a","terms":["a"]},
            {"kind":"documents","question":"b","terms":["b"]}
        ]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/n", "name": "a", "typeRef": "x" }])));
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "2", "path": "/n", "name": "b", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_err(ACTION_START, "no such action /packages/solx-ollama/ollama-chat");
    host.push_done("inv-0", r#"{"responses":[{"text":"found a"}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["responses"].as_array().unwrap().len(), 1);
    assert_eq!(out.output["errors"][0]["kind"], json!("dispatch_error"));
    // Must not have silently fallen back to blocking calls: this failure has
    // nothing to do with host lifetime.
    assert!(!host.call_names().contains(&LLM_REF.to_string()));
}

#[test]
fn two_inquiries_sharing_an_action_fetch_its_param_schema_once() {
    // `search::TypeCache` is built once per `instruct::run` and threaded
    // through every inquiry's `prepare`, precisely so this can happen: two
    // different inquiries surface actions that share a paramTypeRef (or, as
    // here, the very same action - a common helper both questions turn up).
    // Without the shared cache each inquiry's own `run_search_with` would
    // fetch the schema independently.
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[
            {"kind":"actions","question":"how do I search documents?","terms":["search"]},
            {"kind":"actions","question":"how do I look things up?","terms":["lookup"]}
        ]}"#,
    );
    host.push_ok(
        ACTION_SEARCH_REF,
        doc_hits(json!([{ "id": "1", "path": "/builtin/document", "name": "search_documents",
                         "caption": "Search", "paramTypeRef": "/builtin/types/SearchParams" }])),
    );
    host.push_ok(
        ACTION_SEARCH_REF,
        doc_hits(json!([{ "id": "1", "path": "/builtin/document", "name": "search_documents",
                         "caption": "Search", "paramTypeRef": "/builtin/types/SearchParams" }])),
    );
    host.push_ok(TYPE_GET_REF, json!({ "schema": { "type": "object", "properties": { "q": { "type": "string" } } } }));
    host.push_start("inv-0");
    host.push_start("inv-1");
    host.push_done("inv-0", r#"{"scripts":[]}"#);
    host.push_done("inv-1", r#"{"scripts":[]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(host.count(TYPE_GET_REF), 1, "{:?}", host.calls_named(TYPE_GET_REF));
}

// ── console ─────────────────────────────────────────────────────────────────

#[test]
fn milestones_are_printed_with_parseable_tags_and_data() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"what is auth?","terms":["auth"]}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/notes", "name": "auth", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_done("inv-0", r#"{"responses":[{"text":"auth uses tokens"}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());
    assert!(out.success, "{:?}", out.message);

    let prints = host.calls_named(CONSOLE_PRINT);
    let messages: Vec<&str> = prints.iter().map(|p| p["message"].as_str().unwrap()).collect();
    for tag in [
        "[instruct:recall]",
        "[instruct:intent]",
        "[instruct:inquiry:0:terms]",
        "[instruct:inquiry:0:hits]",
        "[instruct:inquiry:0:result]",
        "[instruct:result]",
    ] {
        assert!(messages.iter().any(|m| m.starts_with(tag)), "missing {tag} in {messages:?}");
    }

    // The machine-readable half is what makes a run reconstructable from the
    // console alone, so it must actually carry the data, not just a summary.
    let intent = prints.iter().find(|p| p["message"].as_str().unwrap().starts_with("[instruct:intent]")).unwrap();
    assert_eq!(intent["data"]["inquiries"][0]["question"], json!("what is auth?"));
    let hits = prints.iter().find(|p| p["message"].as_str().unwrap().starts_with("[instruct:inquiry:0:hits]")).unwrap();
    assert_eq!(hits["data"]["refs"], json!(["/notes/auth"]));
}

#[test]
fn child_console_lines_are_drained_per_inquiry_and_other_callers_are_not() {
    // The actual filtering, renumbering and message-prefixing now happen
    // server-side in solx-console's own console/copy (see its own tests) -
    // what this pipeline is responsible for is calling it once per tracked
    // child with the right shape, so a stranger's concurrent output on the
    // same shared console is never even named as a source to copy from.
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[
            {"kind":"documents","question":"a","terms":["a"]},
            {"kind":"documents","question":"b","terms":["b"]}
        ]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/n", "name": "a", "typeRef": "x" }])));
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "2", "path": "/n", "name": "b", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_start("inv-1");
    // The intent call drains the console once on its way through, so the
    // interesting tail has to be queued behind an empty one.
    host.push_ok(CONSOLE_TAIL, json!({ "entries": [], "next_cursor": 0, "first_seq": 0, "dropped": 0 }));
    // The chat action's console is keyed by action_ref alone, so it carries
    // every concurrent caller's output - including a stranger's. `tail`
    // itself is not filtered; the copy calls below are what must be.
    host.push_ok(
        CONSOLE_TAIL,
        json!({
            "entries": [
                console_entry("inv-0", "thinking about a"),
                console_entry("someone-elses-invocation", "not mine"),
                console_entry("inv-1", "thinking about b"),
            ],
            "next_cursor": 3, "first_seq": 0, "dropped": 0,
        }),
    );
    // Queued in the order the fan-out starts its children: inv-0, then inv-1.
    host.push_ok(CONSOLE_COPY, json!({ "copied": 1, "next_cursor": 1 }));
    host.push_ok(CONSOLE_COPY, json!({ "copied": 1, "next_cursor": 1 }));
    host.push_done("inv-0", r#"{"responses":[{"text":"a"}]}"#);
    host.push_done("inv-1", r#"{"responses":[{"text":"b"}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());
    assert!(out.success, "{:?}", out.message);

    // One drain of the intent call's own single-invocation loop, plus one per
    // tracked child on the fan-out's one iteration here: three total, never
    // one call naming "someone-elses-invocation".
    let copies = host.calls_named(CONSOLE_COPY);
    assert_eq!(copies.len(), 3, "{copies:?}");
    assert!(!copies.iter().any(|c| c["invocation_id"] == json!("someone-elses-invocation")), "{copies:?}");
    let child_copies: Vec<&Value> =
        copies.iter().filter(|c| c["invocation_id"] != json!("inv-intent")).collect();
    assert_eq!(child_copies.len(), 2);
    // The label itself is bare (`solx-console`'s console/copy adds the
    // brackets when it prefixes a copied message with it).
    assert!(child_copies.iter().any(|c| c["invocation_id"] == json!("inv-0")
        && c["label"] == json!("instruct:inquiry:0")));
    assert!(child_copies.iter().any(|c| c["invocation_id"] == json!("inv-1")
        && c["label"] == json!("instruct:inquiry:1")));
}

// ── the session document ────────────────────────────────────────────────────

#[test]
fn the_session_is_read_for_history_and_rewritten_with_this_turn_appended() {
    let host = FakeHost::new();
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 40, "offset": 0 }));
    host.push_ok(DOCUMENT_LIST_REF, json!({ "items": [], "total": 0, "limit": 5, "offset": 0 }));
    host.push_ok(
        DOCUMENT_GET_REF,
        json!({
            "id": "1", "path": "/solx-inquiry/sessions", "name": "test", "title": "First question",
            "contents": { "turns": [{ "instruction": "an earlier question", "responses": [{ "text": "an earlier answer" }] }] },
        }),
    );
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"Auth uses tokens."}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());
    assert!(out.success, "{:?}", out.message);

    // History reached the intent prompt.
    let start = host.calls_named(ACTION_START)[0].clone();
    let system = start["params"]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("an earlier question"), "{system}");
    assert!(system.contains("an earlier answer"), "{system}");

    let save = host.calls_named(DOCUMENT_SAVE_REF)[0].clone();
    assert_eq!(save["path"], json!("/solx-inquiry/sessions"));
    assert_eq!(save["name"], json!("test"));
    assert_eq!(save["typeRef"], json!("/packages/solx-inquiry/InstructSession"));
    assert_eq!(save["author"], json!(INSTRUCT_AUTHOR));
    // The existing title is kept, so a session stays findable by what it was
    // originally about.
    assert_eq!(save["title"], json!("First question"));
    let turns = save["contents"]["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "the prior turn must survive, not be replaced");
    assert_eq!(turns[1]["instruction"], base_params()["instruction"]);
    assert_eq!(save["contents"]["turnCount"], json!(2));
}

#[test]
fn a_session_that_cannot_be_written_is_a_warning_not_a_failure() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"Auth uses tokens."}"#);
    host.push_fail(DOCUMENT_SAVE_REF, "disk full");

    let out = run(&host, base_params());

    assert!(out.success, "a completed instruction must not be discarded over its own bookkeeping");
    assert_eq!(out.output["responses"][0]["text"], json!("Auth uses tokens."));
    let warnings = out.output["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("disk full"));
}

// ── recall ──────────────────────────────────────────────────────────────────

#[test]
fn seeded_skills_and_stored_memories_reach_the_prompts() {
    let host = FakeHost::new();
    host.push_ok(
        DOCUMENT_SEARCH_REF,
        json!({
            "items": [
                { "id": "1", "path": "/solx-inquiry/skills", "name": "documents", "title": "Working with documents",
                  "contents": { "scope": "documents", "instructions": "Cite a document by path and name." } },
                { "id": "2", "path": "/solx-inquiry/skills", "name": "solx-scripts", "title": "Writing a .solx script",
                  "contents": { "scope": "actions", "instructions": "Statements are separated by semicolons." } },
            ],
            "total": 2, "limit": 40, "offset": 0,
        }),
    );
    host.push_ok(
        DOCUMENT_LIST_REF,
        json!({
            "items": [{ "id": "3", "path": "/solx-inquiry/memories", "name": "m1", "typeRef": "/packages/solx-inquiry/InquiryMemory", "summary": "auth uses session tokens", "contents": { "text": "auth uses session tokens" } }],
            "total": 1, "limit": 5, "offset": 0,
        }),
    );
    host.push_fail(DOCUMENT_GET_REF, "not found");
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[
            {"kind":"documents","question":"a","terms":["a"]},
            {"kind":"actions","question":"b","terms":["b"]}
        ]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "4", "path": "/n", "name": "a", "typeRef": "x" }])));
    host.push_ok(ACTION_SEARCH_REF, doc_hits(json!([{ "id": "5", "path": "/builtin/document", "name": "search_documents", "caption": "Search" }])));
    host.push_start("inv-0");
    host.push_start("inv-1");
    host.push_done("inv-0", r#"{"responses":[{"text":"a"}]}"#);
    host.push_done("inv-1", r#"{"scripts":[]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());
    assert!(out.success, "{:?}", out.message);

    let starts = host.calls_named(ACTION_START);
    let system = |i: usize| starts[i]["params"]["messages"][0]["content"].as_str().unwrap().to_string();

    // The intent phase has not searched yet, so both scopes are eligible.
    let intent = system(0);
    assert!(intent.contains("Cite a document by path and name."), "{intent}");
    assert!(intent.contains("Statements are separated by semicolons."), "{intent}");
    assert!(intent.contains("auth uses session tokens"), "{intent}");

    // A document inquiry gets the documents skill and the memories, not the
    // scripting one.
    let doc = system(1);
    assert!(doc.contains("Cite a document by path and name."), "{doc}");
    assert!(!doc.contains("Statements are separated by semicolons."), "{doc}");
    assert!(doc.contains("auth uses session tokens"), "{doc}");

    // An action inquiry gets the scripting skill and the primer, and no
    // memories - a prior finding has nothing to say about which action to call.
    let act = system(2);
    assert!(act.contains("Statements are separated by semicolons."), "{act}");
    assert!(!act.contains("Cite a document by path and name."), "{act}");
    assert!(!act.contains("auth uses session tokens"), "{act}");
    assert!(act.contains("Your steps become a .solx script"), "{act}");
}

#[test]
fn caller_chosen_skills_and_memory_paths_are_honoured_on_both_sides() {
    let host = FakeHost::new();
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 40, "offset": 0 }));
    host.push_ok(DOCUMENT_LIST_REF, json!({ "items": [], "total": 0, "limit": 5, "offset": 0 }));
    host.push_fail(DOCUMENT_GET_REF, "not found");
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"q","terms":["fact"]}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/notes", "name": "a", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_done("inv-0", r#"{"responses":[{"text":"a durable fact","memory":true}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let mut params = base_params();
    params["memory_path"] = json!("/team/notes/memories");
    params["skills_path"] = json!("/team/guidance");
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    // Recall reads each from where the caller said it lives.
    assert_eq!(host.calls_named(DOCUMENT_SEARCH_REF)[0]["pathPrefix"], json!("/team/guidance"));
    assert_eq!(host.calls_named(DOCUMENT_LIST_REF)[0]["pathPrefix"], json!("/team/notes/memories"));
    // And a returned memory is stamped with the same path it will be recalled
    // from - the two must not be able to disagree.
    assert_eq!(out.output["memories"][0]["path"], json!("/team/notes/memories"));
}

#[test]
fn omitting_memory_path_turns_memories_off_on_both_sides() {
    let host = FakeHost::new();
    // Only ONE recall search is queued: with memories off there is no second
    // lookup to make, and a stray call would exhaust the FakeHost loudly.
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 40, "offset": 0 }));
    host.push_fail(DOCUMENT_GET_REF, "not found");
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"q","terms":["fact"]}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/notes", "name": "a", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_done("inv-0", r#"{"responses":[{"text":"a durable fact","memory":true}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let mut params = base_params();
    params.as_object_mut().unwrap().remove("memory_path");
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    assert_eq!(host.count(DOCUMENT_LIST_REF), 0, "no memory lookup at all");
    assert_eq!(out.output["memories"], json!([]));
    // The model's judgement still rides on the response - it is what tells a
    // caller that switching memories on would have been worth something.
    assert_eq!(out.output["responses"][0]["memory"], json!(true));
    assert_eq!(out.output["responses"][0]["inquiry"], json!(0), "grounded, so only the path was missing");
    // And that is said out loud, so an empty memories list does not read as
    // "the model judged nothing worth keeping".
    let notes = out.output["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].as_str().unwrap().contains("no memory_path"), "{notes:?}");
}

#[test]
fn memories_off_with_nothing_flagged_says_nothing() {
    let host = FakeHost::new();
    host.push_ok(DOCUMENT_SEARCH_REF, json!({ "items": [], "total": 0, "limit": 40, "offset": 0 }));
    host.push_fail(DOCUMENT_GET_REF, "not found");
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"nothing durable here"}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let mut params = base_params();
    params.as_object_mut().unwrap().remove("memory_path");
    let out = run(&host, params);

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["notes"], json!([]), "no note when nothing was lost");
}

#[test]
fn recall_failing_does_not_stop_the_run() {
    // Reference material is not the answer. A fresh install has no skills
    // path at all, and that must be an ordinary first run.
    let host = FakeHost::new();
    host.push_fail(DOCUMENT_SEARCH_REF, "no such path");
    host.push_fail(DOCUMENT_LIST_REF, "no such path");
    host.push_fail(DOCUMENT_GET_REF, "not found");
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"Auth uses tokens."}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["responses"][0]["text"], json!("Auth uses tokens."));
}

// ── inquiry inputs ──────────────────────────────────────────────────────────

#[test]
fn an_action_inquiry_carries_the_parameter_schema_into_its_prompt() {
    // Without this, the model is asked for correct parameters while being
    // shown only the name of the type that describes them.
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"actions","question":"how do I search?","terms":["search"]}]}"#,
    );
    host.push_ok(
        ACTION_SEARCH_REF,
        doc_hits(json!([{ "id": "1", "path": "/builtin/document", "name": "search_documents",
                         "caption": "Search", "paramTypeRef": "/builtin/types/SearchDocumentsParams" }])),
    );
    host.push_ok(TYPE_GET_REF, json!({ "schema": { "type": "object", "properties": { "q": { "type": "string" } } } }));
    host.push_start("inv-0");
    host.push_done("inv-0", r#"{"scripts":[]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let out = run(&host, base_params());
    assert!(out.success, "{:?}", out.message);

    let user = host.calls_named(ACTION_START)[1]["params"]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(user.contains("paramSchema"), "{user}");
    assert!(user.contains("Available actions"), "{user}");
}

#[test]
fn an_intent_amendment_is_appended_to_the_default_never_substituted_for_it() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"a","terms":["a"],"prompt":"focus on the login flow"}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/n", "name": "a", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_done("inv-0", r#"{"responses":[{"text":"a"}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    run(&host, base_params());

    let system = host.calls_named(ACTION_START)[1]["params"]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(system.contains("focus on the login flow"), "{system}");
    // The grounding rule the model must not be able to talk the pipeline out
    // of is still there.
    assert!(system.contains("using only the search results supplied below"), "{system}");
}

#[test]
fn caller_prompt_overrides_replace_the_defaults() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"a","terms":["a"]}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/n", "name": "a", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_done("inv-0", r#"{"responses":[{"text":"a"}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let mut params = base_params();
    params["intent_prompt"] = json!("custom intent prompt");
    params["document_prompt"] = json!("custom document prompt");
    run(&host, params);

    let starts = host.calls_named(ACTION_START);
    assert!(starts[0]["params"]["messages"][0]["content"].as_str().unwrap().starts_with("custom intent prompt"));
    assert!(starts[1]["params"]["messages"][0]["content"].as_str().unwrap().starts_with("custom document prompt"));
}

#[test]
fn max_inquiries_cannot_be_raised_past_three() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"ok"}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let mut params = base_params();
    params["max_inquiries"] = json!(25);
    run(&host, params);

    let format = host.calls_named(ACTION_START)[0]["params"]["format"].clone();
    assert_eq!(format["properties"]["inquiries"]["maxItems"], json!(3));
}

#[test]
fn connection_overrides_are_forwarded_to_every_call_including_the_fanned_out_ones() {
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done(
        "inv-intent",
        r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"a","terms":["a"]}]}"#,
    );
    host.push_ok(DOCUMENT_SEARCH_REF, doc_hits(json!([{ "id": "1", "path": "/n", "name": "a", "typeRef": "x" }])));
    host.push_start("inv-0");
    host.push_done("inv-0", r#"{"responses":[{"text":"a"}]}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    let mut params = base_params();
    params["base_url"] = json!("http://box:9999");
    params["timeout_secs"] = json!(30);
    params["options"] = json!({ "num_ctx": 8192 });
    run(&host, params);

    for start in host.calls_named(ACTION_START) {
        assert_eq!(start["params"]["base_url"], json!("http://box:9999"));
        assert_eq!(start["params"]["timeout_secs"], json!(30));
        // Added, not substituted for: every payload already sets its own
        // temperature, and raising num_ctx must not cost the caller that.
        assert_eq!(start["params"]["options"]["num_ctx"], json!(8192));
        assert_eq!(start["params"]["options"]["temperature"], json!(0));
    }
}

// ── params ──────────────────────────────────────────────────────────────────

#[test]
fn missing_params_are_reported_before_anything_is_called() {
    let host = FakeHost::new();

    for missing in ["instruction", "model", "session"] {
        let mut params = base_params();
        params.as_object_mut().unwrap().remove(missing);
        let out = run(&host, params);
        assert_eq!(kind(&out), "bad_params");
        assert_eq!(out.output["missing"], json!([missing]));
    }

    let mut params = base_params();
    params["session"] = json!("no-slash");
    assert_eq!(kind(&run(&host, params)), "bad_params");

    assert!(host.call_names().is_empty(), "a param failure must not reach any nested action");
}

#[test]
fn recall_and_the_session_read_happen_before_the_first_llm_call() {
    // Both feed the intent prompt, so getting this order wrong would silently
    // produce an intent decision made without any of the context.
    let host = FakeHost::new();
    push_empty_context(&host);
    host.push_start("inv-intent");
    host.push_done("inv-intent", r#"{"mode":"direct","response":"ok"}"#);
    host.push_ok(DOCUMENT_SAVE_REF, json!({ "id": "1" }));

    run(&host, base_params());

    let first_search = host.first_index(DOCUMENT_SEARCH_REF).unwrap();
    let session_read = host.first_index(DOCUMENT_GET_REF).unwrap();
    let first_start = host.first_index(ACTION_START).unwrap();
    assert!(first_search < first_start);
    assert!(session_read < first_start);
}

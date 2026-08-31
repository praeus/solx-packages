//! Host-target tests for everything except the wit-bindgen shim.
//!
//! `FakeHost` replays canned `exec` responses in order and records every call,
//! so the exact payload handed to `/builtin/web/http_request` can be asserted
//! without a network or a wasm runtime.

use std::cell::RefCell;
use std::collections::VecDeque;

use serde_json::{json, Value};

use solx_ollama::config::{self, normalize_base_url};
use solx_ollama::endpoint::{self, TIMEOUT_HEADROOM_SECS};
use solx_ollama::host::{Host, HostCall, Outcome};

// ── fake host ────────────────────────────────────────────────────────────────

struct FakeHost {
    calls: RefCell<Vec<(String, Value)>>,
    responses: RefCell<VecDeque<Result<HostCall, String>>>,
}

impl FakeHost {
    fn new() -> Self {
        FakeHost {
            calls: RefCell::new(Vec::new()),
            responses: RefCell::new(VecDeque::new()),
        }
    }

    /// Queue a successful `exec` returning `result`.
    fn push_ok(&self, result: Value) -> &Self {
        self.responses.borrow_mut().push_back(Ok(HostCall {
            success: true,
            message: None,
            result,
        }));
        self
    }

    /// Queue a callee that reported failure (wasm/script shape).
    fn push_fail(&self, message: &str) -> &Self {
        self.responses.borrow_mut().push_back(Ok(HostCall {
            success: false,
            message: Some(message.to_string()),
            result: Value::Null,
        }));
        self
    }

    /// Queue a host-level rejection (internal-action shape).
    fn push_err(&self, message: &str) -> &Self {
        self.responses
            .borrow_mut()
            .push_back(Err(message.to_string()));
        self
    }

    /// Queue a `get_env` miss, which is a *success* returning a null value.
    fn push_env_miss(&self) -> &Self {
        self.push_ok(json!({ "value": Value::Null }))
    }

    /// Queue the `get_secret` "no key configured" rejection.
    fn push_secret_unconfigured(&self) -> &Self {
        self.push_err("no key configured for secret OLLAMA_API_KEY")
    }

    /// Queue a canned HTTP response in `http_request`'s output shape.
    fn push_http(&self, status: u64, body: &str) -> &Self {
        self.push_ok(json!({
            "status": status,
            "url": "http://localhost:11434/x",
            "content_type": "application/json",
            "headers": {},
            "body": body,
            "body_encoding": "utf8",
        }))
    }

    /// Queue the full call sequence a streaming endpoint drives through
    /// `/builtin/web/stream/*`: `start` (returning `status`), one
    /// cancellation check (not cancelled), one `poll` returning every line
    /// of `ndjson` as chunks with `done: true`, one `console/print` per
    /// chunk, then `close`. Mirrors `push_http`'s ergonomics for the
    /// common case of "one poll drains the whole response".
    fn push_stream(&self, status: u64, ndjson: &str) -> &Self {
        let chunks: Vec<Value> = ndjson
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("test NDJSON line must be valid JSON"))
            .collect();
        self.push_ok(json!({ "stream_id": "test-stream", "status": status }));
        self.push_ok(json!({ "cancelled": false }));
        self.push_ok(json!({
            "chunks": chunks,
            "next_cursor": chunks.len(),
            "done": true,
            "dropped": 0,
            "status": status,
            "error": Value::Null,
        }));
        for _ in &chunks {
            self.push_ok(json!({ "seq": 1 }));
        }
        self.push_ok(json!({ "closed": true }));
        self
    }

    fn call_named(&self, name: &str) -> Option<Value> {
        self.calls
            .borrow()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p.clone())
    }

    fn calls_named(&self, name: &str) -> Vec<Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// The payload sent to whichever HTTP built-in the call actually used —
    /// the one-shot `/builtin/web/http_request` for a blocking endpoint, or
    /// `/builtin/web/stream/start` for a streaming one. Both take the same
    /// `{url, method, headers, timeout_secs, body?}` shape, so most request-
    /// shape assertions don't need to know which path was taken.
    fn http_payload(&self) -> Value {
        self.call_named("/builtin/web/http_request")
            .or_else(|| self.call_named("/builtin/web/stream/start"))
            .expect("no /builtin/web/http_request or /builtin/web/stream/start call was made")
    }

    /// The JSON request body actually sent to Ollama.
    fn http_body(&self) -> Value {
        let payload = self.http_payload();
        let raw = payload
            .get("body")
            .and_then(Value::as_str)
            .expect("request had no body");
        serde_json::from_str(raw).expect("request body was not JSON")
    }

    fn call_names(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|(n, _)| n.clone()).collect()
    }
}

impl Host for FakeHost {
    fn exec(&self, action_ref: &str, payload: &Value) -> Result<HostCall, String> {
        self.calls
            .borrow_mut()
            .push((action_ref.to_string(), payload.clone()));
        self.responses
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| panic!("FakeHost ran out of responses at {action_ref}"))
    }

    fn log(&self, _msg: &str) {}
}

/// The common prelude for a plain unauthenticated local call: `get_env` for
/// the base URL misses, `get_secret` has no key, `get_env` for the token
/// misses.
fn queue_no_config(host: &FakeHost) {
    host.push_env_miss();
    host.push_secret_unconfigured();
    host.push_env_miss();
}

fn run(host: &FakeHost, fn_name: &str, params: Value) -> Outcome {
    solx_ollama::dispatch(host, Some(fn_name), &params.to_string())
}

fn kind(outcome: &Outcome) -> String {
    outcome
        .output
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("<none>")
        .to_string()
}

// ── base url normalization ───────────────────────────────────────────────────

#[test]
fn normalizes_base_urls() {
    let cases = [
        ("127.0.0.1:11434", "http://127.0.0.1:11434"),
        ("localhost", "http://localhost:11434"),
        // 0.0.0.0 is how ollama serve is told to bind, not an address to dial.
        ("0.0.0.0:11434", "http://127.0.0.1:11434"),
        ("0.0.0.0", "http://127.0.0.1:11434"),
        ("http://box:1234/", "http://box:1234"),
        // An explicit scheme means the author knew what they wanted; never
        // append a port.
        ("https://ollama.com", "https://ollama.com"),
        ("https://ollama.com/", "https://ollama.com"),
        ("http://localhost:11434", "http://localhost:11434"),
        ("  http://a  ", "http://a"),
        ("", "http://localhost:11434"),
        ("   ", "http://localhost:11434"),
        // A path prefix (reverse proxy) survives.
        ("https://gw.example.com/ollama", "https://gw.example.com/ollama"),
    ];
    for (input, want) in cases {
        assert_eq!(normalize_base_url(input), want, "input {input:?}");
    }
}

#[test]
fn base_url_precedence_params_over_env() {
    let host = FakeHost::new();
    // No get_env call for the base url — params win before it is consulted.
    host.push_secret_unconfigured();
    host.push_env_miss();
    host.push_http(200, r#"{"models":[]}"#);

    let out = run(&host, "list_models", json!({ "base_url": "http://box:9999" }));

    assert!(out.success, "{:?}", out.message);
    assert_eq!(
        host.http_payload()["url"],
        json!("http://box:9999/api/tags")
    );
    assert!(
        !host
            .calls
            .borrow()
            .iter()
            .any(|(n, p)| n == "/builtin/env/get_env" && p["key"] == "OLLAMA_HOST"),
        "params.base_url should short-circuit the OLLAMA_HOST lookup"
    );
}

#[test]
fn base_url_falls_back_to_env_then_default() {
    let host = FakeHost::new();
    host.push_ok(json!({ "value": "0.0.0.0:11434" }));
    host.push_secret_unconfigured();
    host.push_env_miss();
    host.push_http(200, r#"{"models":[]}"#);

    run(&host, "list_models", json!({}));
    assert_eq!(
        host.http_payload()["url"],
        json!("http://127.0.0.1:11434/api/tags")
    );

    // And with a null env value, the compiled-in default.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_http(200, r#"{"models":[]}"#);

    run(&host, "list_models", json!({}));
    assert_eq!(
        host.http_payload()["url"],
        json!("http://localhost:11434/api/tags")
    );
}

// ── streaming ────────────────────────────────────────────────────────────────

#[test]
fn streaming_endpoints_send_stream_true() {
    for (fn_name, params) in [
        ("chat", json!({ "model": "m", "messages": [] })),
        ("pull_model", json!({ "model": "m" })),
    ] {
        let host = FakeHost::new();
        queue_no_config(&host);
        host.push_stream(200, r#"{"done":true}"#);

        run(&host, fn_name, params);
        assert_eq!(
            host.http_body()["stream"],
            json!(true),
            "{fn_name} must send stream:true and drive http_stream/start"
        );
        assert!(
            host.call_names().contains(&"/builtin/web/stream/start".to_string()),
            "{fn_name} must call http_stream/start"
        );
    }
}

#[test]
fn caller_cannot_re_enable_streaming() {
    // A caller-supplied `stream` is dropped either way — this endpoint
    // always streams, so a caller-supplied `false` can't turn it off.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(200, r#"{"done":true}"#);

    run(
        &host,
        "chat",
        json!({ "model": "m", "messages": [], "stream": false }),
    );

    assert_eq!(
        host.http_body()["stream"],
        json!(true),
        "a caller-supplied stream:false must be dropped, not honoured"
    );
}

#[test]
fn streaming_chunks_are_logged_to_the_console_and_folded_into_one_result() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(
        200,
        "{\"message\":{\"content\":\"Hel\"},\"done\":false}\n\
         {\"message\":{\"content\":\"lo\"},\"done\":true,\"total_duration\":100}",
    );

    let out = run(&host, "chat", json!({ "model": "m", "messages": [] }));

    assert!(out.success, "{:?}", out.message);
    // The two `message.content` deltas are concatenated into one final
    // string, exactly like the old single blocking response used to carry.
    assert_eq!(out.output["message"]["content"], json!("Hello"));
    assert_eq!(out.output["done"], json!(true));
    assert_eq!(out.output["total_duration"], json!(100));

    let prints = host.calls_named("/builtin/console/print");
    assert_eq!(prints.len(), 2, "{prints:?}");
    assert_eq!(prints[0]["message"], json!("Hel"));
    assert_eq!(prints[0]["data"]["message"]["content"], json!("Hel"));
    assert_eq!(prints[1]["message"], json!("lo"));
    assert_eq!(prints[0]["level"], json!("chunk"));

    assert!(host.call_names().contains(&"/builtin/web/stream/close".to_string()));
}

#[test]
fn tool_calls_survive_the_stream_fold() {
    // Ollama emits each tool call complete, in a chunk of its own that carries
    // no text, and never in the final `done` chunk — so folding only
    // `message.content` (as this used to) dropped them entirely and the caller
    // saw an empty answer instead of a call to make.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(
        200,
        "{\"message\":{\"role\":\"assistant\",\"content\":\"Checking\"},\"done\":false}\n\
         {\"message\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[\
           {\"function\":{\"name\":\"get_weather\",\"arguments\":{\"city\":\"Paris\"}}}]},\"done\":false}\n\
         {\"message\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[\
           {\"function\":{\"name\":\"get_time\",\"arguments\":{\"tz\":\"UTC\"}}}]},\"done\":false}\n\
         {\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\"}",
    );

    let out = run(
        &host,
        "chat",
        json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "weather?" }],
            "tools": [{ "type": "function", "function": { "name": "get_weather" } }],
        }),
    );

    assert!(out.success, "{:?}", out.message);
    let calls = out.output["message"]["tool_calls"]
        .as_array()
        .unwrap_or_else(|| panic!("no tool_calls in {}", out.output));
    assert_eq!(calls.len(), 2, "{calls:?}");
    // Arrival order is the call order the model asked for; it is what the
    // caller has to replay when it sends the results back.
    assert_eq!(calls[0]["function"]["name"], json!("get_weather"));
    assert_eq!(calls[0]["function"]["arguments"]["city"], json!("Paris"));
    assert_eq!(calls[1]["function"]["name"], json!("get_time"));
    // The text deltas still fold exactly as before.
    assert_eq!(out.output["message"]["content"], json!("Checking"));
    assert_eq!(out.output["message"]["role"], json!("assistant"));
    assert_eq!(out.output["done_reason"], json!("stop"));

    // `tools` is a passthrough param, so the definitions reach Ollama.
    assert_eq!(
        host.http_body()["tools"][0]["function"]["name"],
        json!("get_weather")
    );
}

#[test]
fn a_tool_call_chunk_is_named_in_the_console() {
    // Its `message.content` is empty, so without naming the call the console
    // line would be blank and a tailing operator would see the model stall.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(
        200,
        "{\"message\":{\"content\":\"\",\"tool_calls\":[\
           {\"function\":{\"name\":\"get_weather\",\"arguments\":{\"city\":\"Paris\"}}}]},\"done\":false}\n\
         {\"message\":{\"content\":\"\"},\"done\":true}",
    );

    run(&host, "chat", json!({ "model": "m", "messages": [] }));

    let prints = host.calls_named("/builtin/console/print");
    assert_eq!(
        prints[0]["message"],
        json!("[tool_call get_weather({\"city\":\"Paris\"})]")
    );
    // The raw chunk is still attached verbatim.
    assert_eq!(
        prints[0]["data"]["message"]["tool_calls"][0]["function"]["name"],
        json!("get_weather")
    );
}

#[test]
fn a_chat_without_tool_calls_carries_no_tool_calls_key() {
    // An empty array is not the shape a non-streaming reply has, and a caller
    // testing truthiness on it would loop forever.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(
        200,
        "{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":true}",
    );

    let out = run(&host, "chat", json!({ "model": "m", "messages": [] }));

    assert!(out.success, "{:?}", out.message);
    assert!(
        out.output["message"].get("tool_calls").is_none(),
        "{}",
        out.output
    );
}

#[test]
fn a_final_chunk_without_a_message_does_not_swallow_the_answer() {
    // Defensive: real Ollama always sends `message` on the last chat chunk,
    // but folding into a missing key used to discard the entire response.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(
        200,
        "{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":false}\n\
         {\"done\":true,\"done_reason\":\"stop\"}",
    );

    let out = run(&host, "chat", json!({ "model": "m", "messages": [] }));

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output["message"]["content"], json!("hi"));
    assert_eq!(out.output["message"]["role"], json!("assistant"));
}

#[test]
fn a_response_with_nothing_to_fold_is_left_verbatim() {
    // The synthesized `message` above must not appear where there was never
    // any text or tool call to hold.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(200, r#"{"done":true,"done_reason":"load"}"#);

    let out = run(&host, "chat", json!({ "model": "m", "messages": [] }));

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output, json!({ "done": true, "done_reason": "load" }));
}

#[test]
fn mid_stream_cancellation_closes_the_stream_and_fails_the_outcome() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_ok(json!({ "stream_id": "s1", "status": 200 }));
    // The cancellation check runs before the first poll, so a cancelled
    // stream never issues one.
    host.push_ok(json!({ "cancelled": true }));
    host.push_ok(json!({ "closed": true }));

    let out = run(&host, "chat", json!({ "model": "m", "messages": [] }));

    assert!(!out.success);
    assert_eq!(kind(&out), "cancelled");
    assert!(
        !host.call_names().contains(&"/builtin/web/stream/poll".to_string()),
        "a stream cancelled before its first poll must not poll"
    );
    assert!(host.call_names().contains(&"/builtin/web/stream/close".to_string()));
}

#[test]
fn streaming_start_non_2xx_status_maps_to_http_status() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_ok(json!({ "stream_id": "s1", "status": 404 }));
    host.push_ok(json!({ "cancelled": false }));
    host.push_ok(json!({
        "chunks": [{ "error": "model \"x\" not found" }],
        "next_cursor": 1,
        "done": true,
        "dropped": 0,
        "status": 404,
        "error": Value::Null,
    }));
    host.push_ok(json!({ "seq": 1 })); // console/print for the one chunk
    host.push_ok(json!({ "closed": true }));

    let out = run(&host, "chat", json!({ "model": "m", "messages": [] }));

    assert!(!out.success);
    assert_eq!(kind(&out), "http_status");
    assert_eq!(out.output["status"], json!(404));
    assert!(out.message.unwrap().contains(r#"model "x" not found"#));
}

#[test]
fn no_passthrough_list_contains_stream() {
    for ep in endpoint::ENDPOINTS {
        assert!(
            !ep.passthrough.contains(&"stream"),
            "{} must not pass `stream` through",
            ep.fn_name
        );
        assert!(
            !ep.required.contains(&"stream"),
            "{} must not require `stream`",
            ep.fn_name
        );
    }
}

// ── request shape ────────────────────────────────────────────────────────────

#[test]
fn get_endpoints_send_no_body_and_no_content_type() {
    for fn_name in ["list_models"] {
        let host = FakeHost::new();
        queue_no_config(&host);
        host.push_http(200, r#"{"models":[]}"#);

        run(&host, fn_name, json!({}));
        let payload = host.http_payload();

        assert_eq!(payload["method"], json!("GET"), "{fn_name}");
        assert!(payload.get("body").is_none(), "{fn_name} sent a body");
        assert!(
            payload["headers"].get("content-type").is_none(),
            "{fn_name} sent a content-type with no body"
        );
    }
}

#[test]
fn passthrough_params_are_forwarded_and_unknown_ones_dropped() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(200, r#"{"done":true}"#);

    run(
        &host,
        "chat",
        json!({
            "model": "m",
            "messages": [],
            "options": { "temperature": 0 },
            "think": true,
            // Handled locally, never forwarded:
            "base_url": "http://localhost:11434",
            "timeout_secs": 42,
            // Not in the table at all:
            "not_a_real_ollama_field": 1,
        }),
    );

    let body = host.http_body();
    assert_eq!(body["model"], json!("m"));
    assert_eq!(body["options"]["temperature"], json!(0));
    assert_eq!(body["think"], json!(true));
    assert!(body.get("base_url").is_none());
    assert!(body.get("timeout_secs").is_none());
    assert!(body.get("not_a_real_ollama_field").is_none());
}

#[test]
fn null_valued_optional_params_are_omitted() {
    // Schemas allow explicit nulls so templated callers are not rejected;
    // forwarding them would make Ollama reject the request instead.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(200, r#"{"done":true}"#);

    run(
        &host,
        "chat",
        json!({ "model": "m", "messages": [], "format": null, "options": null }),
    );

    let body = host.http_body();
    assert!(body.get("format").is_none());
    assert!(body.get("options").is_none());
}

// ── error mapping ────────────────────────────────────────────────────────────

#[test]
fn transport_failure_from_host_err() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_err("http request failed: connection refused");

    let out = run(&host, "list_models", json!({}));

    assert!(!out.success);
    assert_eq!(kind(&out), "transport");
    assert!(out.message.unwrap().contains("connection refused"));
    assert_eq!(out.output["method"], json!("GET"));
}

#[test]
fn transport_failure_from_unsuccessful_call() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_fail("something went wrong");

    let out = run(&host, "list_models", json!({}));

    assert!(!out.success);
    assert_eq!(kind(&out), "transport");
}

#[test]
fn non_2xx_maps_to_http_status_with_ollamas_error_text() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_http(404, r#"{"error":"model \"x\" not found"}"#);

    let out = run(&host, "list_models", json!({}));

    assert!(!out.success);
    assert_eq!(kind(&out), "http_status");
    assert_eq!(out.output["status"], json!(404));
    assert!(out.message.unwrap().contains(r#"model "x" not found"#));
}

#[test]
fn non_2xx_without_an_error_field_echoes_the_body() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_http(502, "<html>bad gateway</html>");

    let out = run(&host, "list_models", json!({}));

    assert_eq!(kind(&out), "http_status");
    assert!(out.message.unwrap().contains("bad gateway"));
    assert!(out.output["body"].as_str().unwrap().contains("bad gateway"));
}

#[test]
fn two_hundred_with_an_error_object_is_a_failure() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_stream(200, r#"{"error":"model requires more system memory"}"#);

    let out = run(&host, "chat", json!({ "model": "m", "messages": [] }));

    assert!(!out.success);
    assert_eq!(kind(&out), "ollama_error");
    assert_eq!(out.output["status"], json!(200));
}

#[test]
fn empty_200_body_becomes_status_success() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_http(200, "");

    let out = run(&host, "list_models", json!({}));

    assert!(out.success, "{:?}", out.message);
    assert_eq!(out.output, json!({ "status": "success" }));
}

#[test]
fn non_utf8_body_is_reported() {
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_ok(json!({
        "status": 200,
        "url": "http://localhost:11434/api/tags",
        "headers": {},
        "body": "AAECAw==",
        "body_encoding": "base64",
    }));

    let out = run(&host, "list_models", json!({}));

    assert!(!out.success);
    assert_eq!(kind(&out), "non_utf8");
}

#[test]
fn missing_required_params_are_named() {
    let host = FakeHost::new();

    let out = run(&host, "chat", json!({}));

    assert!(!out.success);
    assert_eq!(kind(&out), "bad_params");
    assert_eq!(out.output["missing"], json!(["model", "messages"]));
    assert!(
        host.call_names().is_empty(),
        "a param failure must not reach the network"
    );
}

#[test]
fn unknown_and_absent_fn_names() {
    let host = FakeHost::new();
    let out = solx_ollama::dispatch(&host, Some("nope"), "{}");
    assert!(!out.success);
    assert_eq!(kind(&out), "unknown_action");
    assert_eq!(out.output["fn_name"], json!("nope"));
    let known = out.output["known"].as_array().unwrap();
    assert_eq!(known.len(), endpoint::ENDPOINTS.len() + 1);
    assert!(known.contains(&json!("set_api_key")));

    let out = solx_ollama::dispatch(&host, None, "{}");
    assert_eq!(kind(&out), "unknown_action");
}

#[test]
fn malformed_params() {
    let host = FakeHost::new();

    let out = solx_ollama::dispatch(&host, Some("list_models"), "not json");
    assert_eq!(kind(&out), "bad_params");

    let out = solx_ollama::dispatch(&host, Some("list_models"), "[1,2,3]");
    assert_eq!(kind(&out), "bad_params");

    // A bare `null` is how "no arguments" arrives; it must not be an error.
    queue_no_config(&host);
    host.push_http(200, r#"{"models":[]}"#);
    let out = solx_ollama::dispatch(&host, Some("list_models"), "null");
    assert!(out.success, "{:?}", out.message);
}

// ── auth ─────────────────────────────────────────────────────────────────────

#[test]
fn inline_api_key_wins_and_skips_secret_lookup() {
    let host = FakeHost::new();
    host.push_env_miss();
    host.push_http(200, r#"{"models":[]}"#);

    run(&host, "list_models", json!({ "api_key": "sk-inline" }));

    assert_eq!(
        host.http_payload()["headers"]["authorization"],
        json!("Bearer sk-inline")
    );
    assert!(!host.call_names().contains(&"/builtin/secrets/get_secret".to_string()));
}

#[test]
fn convention_secret_is_used_when_present() {
    let host = FakeHost::new();
    host.push_env_miss();
    host.push_ok(json!({ "value": "sk-stored" }));
    host.push_http(200, r#"{"models":[]}"#);

    run(&host, "list_models", json!({}));

    assert_eq!(
        host.http_payload()["headers"]["authorization"],
        json!("Bearer sk-stored")
    );
}

#[test]
fn unconfigured_convention_secret_is_silent() {
    // The zero-config local case: no key, no stored value, no auth header,
    // and crucially no error.
    let host = FakeHost::new();
    queue_no_config(&host);
    host.push_http(200, r#"{"models":[]}"#);

    let out = run(&host, "list_models", json!({}));

    assert!(out.success, "{:?}", out.message);
    assert!(host.http_payload()["headers"].get("authorization").is_none());
}

#[test]
fn configured_but_empty_convention_secret_is_also_silent() {
    let host = FakeHost::new();
    host.push_env_miss();
    // Key configured, nothing stored: get_secret succeeds with a null value.
    host.push_ok(json!({ "value": Value::Null }));
    host.push_env_miss();
    host.push_http(200, r#"{"models":[]}"#);

    let out = run(&host, "list_models", json!({}));

    assert!(out.success, "{:?}", out.message);
    assert!(host.http_payload()["headers"].get("authorization").is_none());
}

#[test]
fn explicit_auth_secret_name_failure_is_fatal() {
    // Asking for a named secret and silently sending the request in the clear
    // would be the wrong default, so this must abort.
    let host = FakeHost::new();
    host.push_env_miss();
    host.push_err("no key configured for secret MY_TOKEN");

    let out = run(&host, "list_models", json!({ "auth_secret_name": "MY_TOKEN" }));

    assert!(!out.success);
    assert_eq!(kind(&out), "auth");
    assert!(!host.call_names().contains(&"/builtin/web/http_request".to_string()));
}

#[test]
fn explicit_auth_secret_name_with_no_stored_value_is_fatal() {
    let host = FakeHost::new();
    host.push_env_miss();
    host.push_ok(json!({ "value": Value::Null }));

    let out = run(&host, "list_models", json!({ "auth_secret_name": "MY_TOKEN" }));

    assert!(!out.success);
    assert_eq!(kind(&out), "auth");
}

#[test]
fn env_token_is_the_last_resort() {
    let host = FakeHost::new();
    host.push_env_miss();
    host.push_secret_unconfigured();
    host.push_ok(json!({ "value": "sk-from-env" }));
    host.push_http(200, r#"{"models":[]}"#);

    run(&host, "list_models", json!({}));

    assert_eq!(
        host.http_payload()["headers"]["authorization"],
        json!("Bearer sk-from-env")
    );
}

#[test]
fn resolved_auth_overrides_a_caller_supplied_authorization_header() {
    let host = FakeHost::new();
    host.push_env_miss();
    host.push_http(200, r#"{"models":[]}"#);

    run(
        &host,
        "list_models",
        json!({
            "api_key": "sk-real",
            "headers": { "Authorization": "Bearer stale", "X-Trace": "abc" },
        }),
    );

    let headers = host.http_payload()["headers"].clone();
    assert_eq!(headers["authorization"], json!("Bearer sk-real"));
    // Other caller headers survive, lowercased.
    assert_eq!(headers["x-trace"], json!("abc"));
}

// ── set_api_key ──────────────────────────────────────────────────────────────

#[test]
fn set_api_key_writes_the_hardcoded_secret_name() {
    let host = FakeHost::new();
    host.push_ok(json!({ "set": true }));

    let out = run(&host, "set_api_key", json!({ "value": "sk-abc", "name": "EVIL" }));

    assert!(out.success, "{:?}", out.message);
    let payload = host.call_named("/builtin/secrets/set_secret").unwrap();
    assert_eq!(payload["name"], json!(config::DEFAULT_SECRET));
    assert_eq!(payload["value"], json!("sk-abc"));
}

#[test]
fn set_api_key_rejects_an_empty_value() {
    let host = FakeHost::new();
    let out = run(&host, "set_api_key", json!({ "value": "   " }));

    assert!(!out.success);
    assert_eq!(kind(&out), "bad_params");
    assert!(host.call_names().is_empty());
}

// ── timeouts ─────────────────────────────────────────────────────────────────

#[test]
fn timeout_is_clamped_to_the_endpoint_ceiling() {
    let chat = endpoint::lookup("chat").unwrap();

    for (given, want) in [
        (Some(999_999), chat.max_timeout),
        (Some(0), 1),
        (Some(45), 45),
        (None, chat.default_timeout),
    ] {
        let host = FakeHost::new();
        queue_no_config(&host);
        host.push_stream(200, r#"{"done":true}"#);

        let mut params = json!({ "model": "m", "messages": [] });
        if let Some(t) = given {
            params["timeout_secs"] = json!(t);
        }
        run(&host, "chat", params);

        assert_eq!(
            host.http_payload()["timeout_secs"],
            json!(want),
            "timeout_secs {given:?}"
        );
    }
}

#[test]
fn outer_timeout_always_exceeds_the_inner_ceiling() {
    // install.solx sets action_config.timeout_secs from outer_timeout(); if
    // that were ever <= max_timeout the wasm budget could fire first and the
    // caller would see "wasm action timed out" instead of a real HTTP error.
    for ep in endpoint::ENDPOINTS {
        assert!(
            ep.max_timeout >= ep.default_timeout,
            "{}: max_timeout < default_timeout",
            ep.fn_name
        );
        assert_eq!(
            ep.outer_timeout(),
            ep.max_timeout + TIMEOUT_HEADROOM_SECS,
            "{}",
            ep.fn_name
        );
        assert!(ep.outer_timeout() > ep.max_timeout, "{}", ep.fn_name);
    }
}

// ── table integrity ──────────────────────────────────────────────────────────

#[test]
fn endpoint_table_is_well_formed() {
    let mut seen = Vec::new();
    for ep in endpoint::ENDPOINTS {
        assert!(
            !seen.contains(&ep.fn_name),
            "duplicate fn_name {}",
            ep.fn_name
        );
        seen.push(ep.fn_name);

        assert_ne!(ep.fn_name, endpoint::SET_API_KEY_FN, "shadows the bootstrap action");
        assert!(ep.path.starts_with("/api/"), "{}", ep.fn_name);
        assert!(
            matches!(ep.method, "GET" | "POST" | "DELETE"),
            "{}: unexpected method {}",
            ep.fn_name,
            ep.method
        );
        // A GET carries no body, so requiring params it cannot send is a bug.
        if ep.method == "GET" {
            assert!(ep.required.is_empty(), "{} is a GET with required params", ep.fn_name);
            assert!(ep.passthrough.is_empty(), "{} is a GET with passthrough params", ep.fn_name);
            assert!(ep.streaming.is_none(), "{} is a GET forcing stream", ep.fn_name);
        }
        // A locally-handled param must never collide with a forwarded one.
        for common in endpoint::COMMON_PARAMS {
            assert!(
                !ep.passthrough.contains(common) && !ep.required.contains(common),
                "{} forwards the locally-handled param {common}",
                ep.fn_name
            );
        }
    }
    assert_eq!(seen.len(), 3, "endpoint count changed - update install.solx");
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

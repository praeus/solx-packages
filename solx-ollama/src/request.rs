//! Issue an endpoint call and turn the response into an [`Outcome`].
//!
//! Two call shapes, chosen by `ep.streaming`:
//!
//! * [`call_blocking`] — the original path, through `/builtin/web/http_request`.
//!   `http_request` treats a non-2xx as a *successful call that returned a
//!   status* — it only errors on transport failure. That split is preserved
//!   here as two distinct failure kinds (`transport` vs `http_status`),
//!   because they mean very different things to a caller: one says the
//!   server is unreachable, the other says the server rejected the request.
//! * [`call_streaming`] — for endpoints Ollama can stream (`chat`,
//!   `pull_model`). Drives the call
//!   through `/builtin/web/stream/*` (`solx-actions`): starts the request,
//!   then repeatedly polls, emitting each NDJSON chunk to the action's own
//!   console and folding it into a running aggregate, checking
//!   `/builtin/action/cancelled` between polls so a `action_stop` on a
//!   detached invocation closes the upstream connection promptly instead of
//!   only being caught by the outer force-abort. Once the stream reports
//!   `done`, the aggregate is handed to the same [`interpret`] the blocking
//!   path uses — so the final [`Outcome`] shape a caller sees is unchanged
//!   from before streaming existed.

use serde_json::{json, Map, Value};

use crate::config;
use crate::endpoint::{self, Endpoint, StreamKind};
use crate::host::{truncate, Host, Outcome};

/// Longest error body echoed back in the failure output.
const MAX_BODY_ECHO: usize = 2048;
/// Longest body used as an error *message* when Ollama sent no `error` field.
const MAX_DETAIL_ECHO: usize = 512;
/// Longest `function.arguments` rendering echoed into a console chunk line.
const MAX_TOOL_ARGS_ECHO: usize = 256;
/// `wait_secs` passed to each `http_stream/poll` — how long to long-poll for
/// the next chunk (or `done`) before looping back around to re-check
/// cancellation.
const POLL_WAIT_SECS: u64 = 5;

pub fn call(host: &dyn Host, ep: &Endpoint, params: &Value) -> Outcome {
    match ep.streaming {
        Some(kind) => call_streaming(host, ep, kind, params),
        None => call_blocking(host, ep, params),
    }
}

/// Everything both call shapes need: the resolved URL, the (possibly
/// legacy-rewritten) path `interpret` should report errors against, and the
/// JSON payload to hand to either `http_request` or `http_stream/start` —
/// both built-ins take the same `{url, method, headers, timeout_secs,
/// body?, body_encoding?}` shape.
struct PreparedRequest {
    url: String,
    path: &'static str,
    timeout: u64,
    payload: Value,
}

fn prepare_request(host: &dyn Host, ep: &Endpoint, params: &Value) -> Result<PreparedRequest, Outcome> {
    let body = endpoint::build_body(ep, params).map_err(|missing| {
        Outcome::fail(
            "bad_params",
            format!("{} requires: {}", ep.fn_name, missing.join(", ")),
            json!({ "missing": missing }),
        )
    })?;
    let path = ep.path;

    let base = config::resolve_base_url(host, params);
    let url = format!("{base}{path}");

    let mut headers = Map::new();
    if body.is_some() {
        headers.insert("content-type".to_string(), json!("application/json"));
    }
    // Caller headers go in before Authorization so our bearer wins a conflict
    // rather than being silently overwritten by a stale one.
    if let Some(h) = params.get("headers").and_then(Value::as_object) {
        for (k, v) in h {
            if v.is_string() {
                headers.insert(k.to_ascii_lowercase(), v.clone());
            }
        }
    }
    match config::resolve_auth(host, params) {
        Ok(Some(token)) => {
            headers.insert("authorization".to_string(), json!(format!("Bearer {token}")));
        }
        Ok(None) => {}
        Err(e) => return Err(Outcome::fail("auth", e, json!({}))),
    }

    let timeout = params
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(ep.default_timeout)
        .clamp(1, ep.max_timeout);

    let mut payload = json!({
        "url": url,
        "method": ep.method,
        "headers": Value::Object(headers),
        "timeout_secs": timeout,
    });
    if let Some(b) = &body {
        payload["body"] = json!(b.to_string());
        payload["body_encoding"] = json!("utf8");
    }

    Ok(PreparedRequest { url, path, timeout, payload })
}

fn call_blocking(host: &dyn Host, ep: &Endpoint, params: &Value) -> Outcome {
    let prepared = match prepare_request(host, ep, params) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };

    host.log(&format!(
        "solx-ollama {} {} (timeout {}s)",
        ep.method, prepared.url, prepared.timeout
    ));

    let call = match host.exec("/builtin/web/http_request", &prepared.payload) {
        Ok(c) => c,
        Err(e) => {
            return Outcome::fail(
                "transport",
                format!("{} {} failed: {e}", ep.method, prepared.url),
                json!({ "url": prepared.url, "method": ep.method }),
            )
        }
    };
    if !call.success {
        return Outcome::fail(
            "transport",
            format!(
                "{} {} failed: {}",
                ep.method,
                prepared.url,
                call.message.unwrap_or_else(|| "no message".to_string())
            ),
            json!({ "url": prepared.url, "method": ep.method }),
        );
    }

    interpret(ep, prepared.path, &prepared.url, &call.result)
}

fn call_streaming(host: &dyn Host, ep: &Endpoint, kind: StreamKind, params: &Value) -> Outcome {
    let prepared = match prepare_request(host, ep, params) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };

    host.log(&format!(
        "solx-ollama {} {} (streaming, timeout {}s)",
        ep.method, prepared.url, prepared.timeout
    ));

    let start = match host.exec("/builtin/web/stream/start", &prepared.payload) {
        Ok(c) => c,
        Err(e) => {
            return Outcome::fail(
                "transport",
                format!("{} {} failed: {e}", ep.method, prepared.url),
                json!({ "url": prepared.url, "method": ep.method }),
            )
        }
    };
    if !start.success {
        return Outcome::fail(
            "transport",
            format!(
                "{} {} failed: {}",
                ep.method,
                prepared.url,
                start.message.unwrap_or_else(|| "no message".to_string())
            ),
            json!({ "url": prepared.url, "method": ep.method }),
        );
    }
    let Some(stream_id) = start.result.get("stream_id").and_then(Value::as_str).map(str::to_string) else {
        return Outcome::fail(
            "transport",
            format!("{} {} did not return a stream_id", ep.method, prepared.url),
            json!({ "url": prepared.url, "method": ep.method }),
        );
    };
    let status = start.result.get("status").and_then(Value::as_u64).unwrap_or(0);

    let mut agg = Aggregate::new(kind);
    let mut cursor: i64 = 0;

    let stream_error: Option<String> = loop {
        if is_cancelled(host) {
            let _ = host.exec("/builtin/web/stream/close", &json!({ "stream_id": stream_id }));
            return Outcome::fail(
                "cancelled",
                format!("{} {} cancelled", ep.method, prepared.url),
                json!({ "url": prepared.url, "method": ep.method }),
            );
        }

        let poll = host.exec(
            "/builtin/web/stream/poll",
            &json!({ "stream_id": stream_id, "cursor": cursor, "wait_secs": POLL_WAIT_SECS }),
        );
        let poll = match poll {
            Ok(c) if c.success => c,
            Ok(c) => {
                let _ = host.exec("/builtin/web/stream/close", &json!({ "stream_id": stream_id }));
                return Outcome::fail(
                    "transport",
                    format!(
                        "{} {} poll failed: {}",
                        ep.method,
                        prepared.url,
                        c.message.unwrap_or_else(|| "no message".to_string())
                    ),
                    json!({ "url": prepared.url, "method": ep.method }),
                );
            }
            Err(e) => {
                let _ = host.exec("/builtin/web/stream/close", &json!({ "stream_id": stream_id }));
                return Outcome::fail(
                    "transport",
                    format!("{} {} poll failed: {e}", ep.method, prepared.url),
                    json!({ "url": prepared.url, "method": ep.method }),
                );
            }
        };

        for chunk in poll.result.get("chunks").and_then(Value::as_array).into_iter().flatten() {
            let message = agg.summarize(chunk);
            let _ = host.exec(
                "/builtin/console/print",
                &json!({ "level": "chunk", "message": message, "data": chunk }),
            );
            agg.fold(chunk);
        }
        cursor = poll.result.get("next_cursor").and_then(Value::as_i64).unwrap_or(cursor);

        if poll.result.get("done").and_then(Value::as_bool) == Some(true) {
            break poll.result.get("error").and_then(Value::as_str).map(str::to_string);
        }
    };

    let _ = host.exec("/builtin/web/stream/close", &json!({ "stream_id": stream_id }));

    if let Some(err) = stream_error {
        return Outcome::fail(
            "transport",
            format!("{} {} failed: {err}", ep.method, prepared.url),
            json!({ "url": prepared.url, "method": ep.method }),
        );
    }

    // Reuse the exact same status/body interpretation the blocking path
    // uses, by handing it a synthetic `http_request`-shaped response built
    // from the aggregated stream — this is what keeps the caller-facing
    // `Outcome` shape identical to the pre-streaming call.
    let synthetic = json!({
        "status": status,
        "body": agg.finish().to_string(),
        "body_encoding": "utf8",
    });
    interpret(ep, prepared.path, &prepared.url, &synthetic)
}

/// Best-effort: a failure to check cancellation (host rejection, or a
/// callee that returned `success: false`, e.g. because there is no action
/// caller in some non-action test harness) is treated as "not cancelled"
/// rather than aborting the stream — the outer `action_stop` force-abort
/// remains the backstop either way.
fn is_cancelled(host: &dyn Host) -> bool {
    match host.exec("/builtin/action/cancelled", &json!({})) {
        Ok(c) if c.success => c.result.get("cancelled").and_then(Value::as_bool).unwrap_or(false),
        _ => false,
    }
}

/// Folds a streaming endpoint's chunks into one aggregate value shaped like
/// the endpoint's non-streaming response, and produces the short per-chunk
/// text `call_streaming` logs to the console.
struct Aggregate {
    kind: StreamKind,
    content: String,
    thinking: String,
    /// Every `message.tool_calls` entry seen across the whole stream, in
    /// arrival order. Ollama emits a tool call as a *complete* object in
    /// whichever chunk it finishes parsing it in — usually one that carries
    /// no `content` at all, and never the final `done` chunk — so the only
    /// way to keep them is to collect them as they go and put them back in
    /// [`Aggregate::finish`].
    tool_calls: Vec<Value>,
    last: Value,
}

impl Aggregate {
    fn new(kind: StreamKind) -> Self {
        Aggregate {
            kind,
            content: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            last: Value::Null,
        }
    }

    fn summarize(&self, chunk: &Value) -> String {
        match self.kind {
            // A tool-call chunk carries no text, so without this the console
            // line for it would be empty and a tailing operator would see the
            // model apparently stall mid-answer.
            StreamKind::Chat => {
                let mut line = chunk
                    .pointer("/message/content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                for call in tool_calls_of(chunk) {
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    let args = call
                        .pointer("/function/arguments")
                        .map(|a| truncate(&a.to_string(), MAX_TOOL_ARGS_ECHO))
                        .unwrap_or_default();
                    line.push_str(&format!("[tool_call {name}({args})]"));
                }
                line
            }
            StreamKind::Progress => {
                let status = chunk.get("status").and_then(Value::as_str).unwrap_or("");
                match (
                    chunk.get("completed").and_then(Value::as_u64),
                    chunk.get("total").and_then(Value::as_u64),
                ) {
                    (Some(c), Some(t)) => format!("{status} ({c}/{t})"),
                    _ => status.to_string(),
                }
            }
        }
    }

    fn fold(&mut self, chunk: &Value) {
        match self.kind {
            StreamKind::Chat => {
                if let Some(s) = chunk.pointer("/message/content").and_then(Value::as_str) {
                    self.content.push_str(s);
                }
                if let Some(s) = chunk.pointer("/message/thinking").and_then(Value::as_str) {
                    self.thinking.push_str(s);
                }
                self.tool_calls.extend(tool_calls_of(chunk).cloned());
            }
            StreamKind::Progress => {}
        }
        self.last = chunk.clone();
    }

    /// The final chunk's own object (carrying `done`/stats/`error`/etc.),
    /// with the concatenated text and every tool call collected along the way
    /// folded back in — matching exactly what a non-streaming call to the same
    /// endpoint would have returned.
    fn finish(self) -> Value {
        let Aggregate { kind, content, thinking, tool_calls, last } = self;
        let mut out = if last.is_object() { last } else { json!({}) };
        if let Some(obj) = out.as_object_mut() {
            match kind {
                StreamKind::Chat => {
                    // The final chunk always carries a `message` object. If a
                    // server ever omits it, synthesize one rather than dropping
                    // the whole answer on the floor — but only when there is
                    // something to put in it, so a response with no message at
                    // all still comes back verbatim.
                    let has_message = obj.get("message").is_some_and(Value::is_object);
                    let has_payload =
                        !content.is_empty() || !thinking.is_empty() || !tool_calls.is_empty();
                    let msg = if has_message || has_payload {
                        let msg = obj.entry("message").or_insert_with(|| json!({}));
                        if !msg.is_object() {
                            *msg = json!({});
                        }
                        msg.as_object_mut()
                    } else {
                        None
                    };
                    if let Some(msg) = msg {
                        msg.entry("role").or_insert_with(|| json!("assistant"));
                        msg.insert("content".to_string(), json!(content));
                        if !thinking.is_empty() {
                            msg.insert("thinking".to_string(), json!(thinking));
                        }
                        // Absent rather than empty when the model called
                        // nothing — that is the shape a non-streaming reply
                        // has.
                        if !tool_calls.is_empty() {
                            msg.insert("tool_calls".to_string(), Value::Array(tool_calls));
                        }
                    }
                }
                StreamKind::Progress => {}
            }
        }
        out
    }
}

/// The `message.tool_calls` entries of one chunk, empty for a chunk that has
/// none (or whose `tool_calls` is not an array).
fn tool_calls_of(chunk: &Value) -> std::slice::Iter<'_, Value> {
    const NONE: &[Value] = &[];
    chunk
        .pointer("/message/tool_calls")
        .and_then(Value::as_array)
        .map_or(NONE, Vec::as_slice)
        .iter()
}

fn interpret(ep: &Endpoint, path: &str, url: &str, resp: &Value) -> Outcome {
    let status = resp.get("status").and_then(Value::as_u64).unwrap_or(0);

    // A base64 body means the bytes were not valid UTF-8, so they are
    // certainly not the JSON we expect. Report it rather than silently
    // returning null.
    if resp.get("body_encoding").and_then(Value::as_str) == Some("base64") {
        return Outcome::fail(
            "non_utf8",
            format!("{url} returned a non-UTF-8 body (status {status})"),
            json!({ "status": status, "url": url }),
        );
    }

    let raw = resp.get("body").and_then(Value::as_str).unwrap_or("");
    let parsed: Value = if raw.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(raw).unwrap_or(Value::Null)
    };

    if !(200..300).contains(&status) {
        let detail = parsed
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| truncate(raw, MAX_DETAIL_ECHO));
        return Outcome::fail(
            "http_status",
            format!("ollama {} {path} returned {status}: {detail}", ep.method),
            json!({
                "status": status,
                "url": url,
                "body": truncate(raw, MAX_BODY_ECHO),
            }),
        );
    }

    // Ollama sometimes answers 200 with an error object in the body.
    if let Some(e) = parsed.get("error").and_then(Value::as_str) {
        return Outcome::fail(
            "ollama_error",
            format!("ollama {} {path}: {e}", ep.method),
            json!({ "status": status, "url": url }),
        );
    }

    // Some Ollama endpoints answer 200 with an empty body.
    Outcome::ok(if parsed.is_null() {
        json!({ "status": "success" })
    } else {
        parsed
    })
}

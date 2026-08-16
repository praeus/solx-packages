//! Issue an endpoint call and turn the response into an [`Outcome`].
//!
//! Two call shapes, chosen by `ep.streaming`:
//!
//! * [`call_blocking`] — the original path, through `/builtin/http_request`.
//!   `http_request` treats a non-2xx as a *successful call that returned a
//!   status* — it only errors on transport failure. That split is preserved
//!   here as two distinct failure kinds (`transport` vs `http_status`),
//!   because they mean very different things to a caller: one says the
//!   server is unreachable, the other says the server rejected the request.
//! * [`call_streaming`] — for endpoints Ollama can stream (`generate`,
//!   `chat`, `pull_model`, `push_model`, `create_model`). Drives the call
//!   through `/builtin/http_stream/*` (`solx-actions`): starts the request,
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
use crate::endpoint::{self, Endpoint, StreamKind, LEGACY_EMBED_PATH};
use crate::host::{truncate, Host, Outcome};

/// Longest error body echoed back in the failure output.
const MAX_BODY_ECHO: usize = 2048;
/// Longest body used as an error *message* when Ollama sent no `error` field.
const MAX_DETAIL_ECHO: usize = 512;
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
    let mut body = endpoint::build_body(ep, params).map_err(|missing| {
        Outcome::fail(
            "bad_params",
            format!("{} requires: {}", ep.fn_name, missing.join(", ")),
            json!({ "missing": missing }),
        )
    })?;

    // `legacy: true` on `embed` retargets the older single-prompt endpoint,
    // which names the field `prompt` and accepts only a string.
    let mut path = ep.path;
    if ep.fn_name == "embed" && params.get("legacy").and_then(Value::as_bool) == Some(true) {
        match rewrite_legacy_embed(&mut body) {
            Ok(()) => path = LEGACY_EMBED_PATH,
            Err(e) => return Err(Outcome::fail("bad_params", e, json!({}))),
        }
    }

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

    let call = match host.exec("/builtin/http_request", &prepared.payload) {
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

    let start = match host.exec("/builtin/http_stream/start", &prepared.payload) {
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
            let _ = host.exec("/builtin/http_stream/close", &json!({ "stream_id": stream_id }));
            return Outcome::fail(
                "cancelled",
                format!("{} {} cancelled", ep.method, prepared.url),
                json!({ "url": prepared.url, "method": ep.method }),
            );
        }

        let poll = host.exec(
            "/builtin/http_stream/poll",
            &json!({ "stream_id": stream_id, "cursor": cursor, "wait_secs": POLL_WAIT_SECS }),
        );
        let poll = match poll {
            Ok(c) if c.success => c,
            Ok(c) => {
                let _ = host.exec("/builtin/http_stream/close", &json!({ "stream_id": stream_id }));
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
                let _ = host.exec("/builtin/http_stream/close", &json!({ "stream_id": stream_id }));
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

    let _ = host.exec("/builtin/http_stream/close", &json!({ "stream_id": stream_id }));

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
    response: String,
    content: String,
    thinking: String,
    last: Value,
}

impl Aggregate {
    fn new(kind: StreamKind) -> Self {
        Aggregate {
            kind,
            response: String::new(),
            content: String::new(),
            thinking: String::new(),
            last: Value::Null,
        }
    }

    fn summarize(&self, chunk: &Value) -> String {
        match self.kind {
            StreamKind::Generate => chunk.get("response").and_then(Value::as_str).unwrap_or("").to_string(),
            StreamKind::Chat => chunk
                .pointer("/message/content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
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
            StreamKind::Generate => {
                if let Some(s) = chunk.get("response").and_then(Value::as_str) {
                    self.response.push_str(s);
                }
            }
            StreamKind::Chat => {
                if let Some(s) = chunk.pointer("/message/content").and_then(Value::as_str) {
                    self.content.push_str(s);
                }
                if let Some(s) = chunk.pointer("/message/thinking").and_then(Value::as_str) {
                    self.thinking.push_str(s);
                }
            }
            StreamKind::Progress => {}
        }
        self.last = chunk.clone();
    }

    /// The final chunk's own object (carrying `done`/stats/`error`/etc.),
    /// with the concatenated text folded back in — matching exactly what a
    /// non-streaming call to the same endpoint used to return.
    fn finish(self) -> Value {
        let mut out = if self.last.is_object() { self.last } else { json!({}) };
        if let Some(obj) = out.as_object_mut() {
            match self.kind {
                StreamKind::Generate => {
                    obj.insert("response".to_string(), json!(self.response));
                }
                StreamKind::Chat => {
                    if let Some(msg) = obj.get_mut("message").and_then(Value::as_object_mut) {
                        msg.insert("content".to_string(), json!(self.content));
                        if !self.thinking.is_empty() {
                            msg.insert("thinking".to_string(), json!(self.thinking));
                        }
                    }
                }
                StreamKind::Progress => {}
            }
        }
        out
    }
}

/// `/api/embeddings` takes `prompt` (a single string) where `/api/embed`
/// takes `input` (string or array).
fn rewrite_legacy_embed(body: &mut Option<Value>) -> Result<(), String> {
    let Some(obj) = body.as_mut().and_then(Value::as_object_mut) else {
        return Err("embed: internal error, no request body to rewrite".to_string());
    };
    let input = obj.remove("input").unwrap_or(Value::Null);
    match input {
        Value::String(s) => {
            obj.insert("prompt".to_string(), Value::String(s));
            obj.remove("dimensions");
            obj.remove("truncate");
            Ok(())
        }
        Value::Array(_) => Err(
            "embed: legacy mode targets /api/embeddings, which accepts a single \
             prompt string - pass a string input or drop legacy"
                .to_string(),
        ),
        _ => Err("embed: input must be a string in legacy mode".to_string()),
    }
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

    // /api/copy and /api/delete answer 200 with an empty body.
    Outcome::ok(if parsed.is_null() {
        json!({ "status": "success" })
    } else {
        parsed
    })
}

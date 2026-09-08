//! Drive one llm-action call (`ollama-chat`, by default) so that, when the
//! host supports it, the call runs detached and this pipeline can echo its
//! progress into `inquire`'s own console and cooperatively cancel it.
//!
//! [`call`] tries `/builtin/action/start` first. Two things can send it down
//! the plain blocking `exec` path instead:
//!
//! * **The host isn't long-lived.** `action_start` refuses under a bare
//!   `solx exec` (the process exits the instant `exec` returns, which would
//!   kill a spawned task before anyone could poll it) — only `solx-server`/
//!   `solx-mcp`, or a CLI proxied to one, allow it. Detecting this by message
//!   text is fragile but there is no error code on the WIT boundary to
//!   switch on instead, and the message is a stable, deliberately-worded
//!   refusal (`solx-actions/src/lib.rs::start_invocation`), not incidental
//!   text.
//! * **`llm_action_ref` isn't a `/path/name` reference** (`split_ref` fails)
//!   — falls back so a caller who only ever overrides it with a bare action
//!   name-shaped string still gets *something* to work with instead of a
//!   hard failure. Given the default is always well-formed, this only bites
//!   a caller who passed a malformed override.
//!
//! Once started, [`poll_to_completion`] loops: check `inquire`'s own
//! cancellation first (so a cancelled pipeline doesn't have to sit out a
//! whole poll wait), long-poll the child's status, then echo whatever
//! console output it produced meanwhile — mirroring the check/wait/drain
//! shape `solx-ollama`'s own HTTP-stream loop already uses, one layer up.

use serde_json::{json, Value};

use crate::host::{split_ref, Host, Outcome};
use crate::params::Params;

/// How long each `action_poll` long-polls before this loop wakes up to
/// re-check `inquire`'s own cancellation and drain console output. Mirrors
/// `solx-ollama`'s own `POLL_WAIT_SECS` for its HTTP-stream loop.
const POLL_WAIT_SECS: u64 = 5;

/// Substring of `solx-actions`' refusal message
/// (`"action_start requires a long-lived host ..."`) used to distinguish
/// "this host can't detach calls" from a genuine dispatch failure.
const LONG_LIVED_HOST_MARKER: &str = "long-lived host";

const TERMINAL_OK: &str = "ok";
const TERMINAL_FAILED: &str = "failed";
const TERMINAL_CANCELLED: &str = "cancelled";
const TERMINAL_TIMEOUT: &str = "timeout";
const TERMINAL_INTERRUPTED: &str = "interrupted";

fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        TERMINAL_OK | TERMINAL_FAILED | TERMINAL_CANCELLED | TERMINAL_TIMEOUT | TERMINAL_INTERRUPTED
    )
}

/// Run one llm-action call and return its `result` value (the same shape a
/// plain `exec` of that action would have returned as `HostCall::result`).
///
/// `stage` is `"terms"` or `"summary"` — folded into every error and into
/// the prefix on echoed console lines, so an operator watching `inquire`'s
/// console can tell which phase a line belongs to.
pub fn call(host: &dyn Host, p: &Params, payload: Value, stage: &'static str) -> Result<Value, Outcome> {
    let Some((path, name)) = split_ref(&p.llm_action_ref) else {
        return call_blocking(host, p, &payload, stage);
    };

    let start = host.exec(
        "/builtin/action/start",
        &json!({ "path": path, "name": name, "params": payload }),
    );
    match start {
        Ok(c) if c.success => poll_to_completion(host, &c.result, stage),
        Ok(c) => fall_back_or_fail(host, p, &payload, stage, c.message.unwrap_or_default()),
        Err(e) => fall_back_or_fail(host, p, &payload, stage, e),
    }
}

fn fall_back_or_fail(
    host: &dyn Host,
    p: &Params,
    payload: &Value,
    stage: &'static str,
    message: String,
) -> Result<Value, Outcome> {
    if message.contains(LONG_LIVED_HOST_MARKER) {
        return call_blocking(host, p, payload, stage);
    }
    Err(Outcome::fail(
        "dispatch_error",
        format!("action_start failed ({stage}): {message}"),
        json!({ "stage": stage, "action_ref": p.llm_action_ref }),
    ))
}

/// The plain, single blocking `exec` path — used when the host can't detach
/// calls, or `llm_action_ref` doesn't parse as a `/path/name` reference.
fn call_blocking(host: &dyn Host, p: &Params, payload: &Value, stage: &'static str) -> Result<Value, Outcome> {
    let call = host
        .exec(&p.llm_action_ref, payload)
        .map_err(|e| dispatch_failure(p, stage, e))?;
    if !call.success {
        return Err(llm_failure(p, stage, call.message, call.result));
    }
    Ok(call.result)
}

fn poll_to_completion(host: &dyn Host, start_result: &Value, stage: &'static str) -> Result<Value, Outcome> {
    let invocation_id = start_result
        .get("invocation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Outcome::fail(
                "dispatch_error",
                "action_start returned no invocation_id",
                json!({ "stage": stage }),
            )
        })?
        .to_string();
    let action_ref = start_result
        .get("action_ref")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut cursor = start_result.get("console_seq_start").and_then(Value::as_i64).unwrap_or(0);

    loop {
        if own_invocation_cancelled(host) {
            let _ = host.exec("/builtin/action/stop", &json!({ "invocation_id": invocation_id }));
            return Err(Outcome::fail(
                "cancelled",
                format!("inquire was cancelled during {stage}"),
                json!({ "stage": stage, "invocation_id": invocation_id }),
            ));
        }

        let poll = host
            .exec(
                "/builtin/action/poll",
                &json!({ "invocation_id": invocation_id, "wait_secs": POLL_WAIT_SECS }),
            )
            .map_err(|e| {
                Outcome::fail(
                    "dispatch_error",
                    format!("action_poll failed ({stage}): {e}"),
                    json!({ "stage": stage, "invocation_id": invocation_id }),
                )
            })?;
        if !poll.success {
            return Err(Outcome::fail(
                "dispatch_error",
                format!("action_poll failed ({stage}): {}", poll.message.unwrap_or_default()),
                json!({ "stage": stage, "invocation_id": invocation_id }),
            ));
        }

        cursor = echo_console(host, &action_ref, &invocation_id, cursor, stage);

        let status = poll.result.get("status").and_then(Value::as_str).unwrap_or("");
        if is_terminal(status) {
            return finish(status, &poll.result, stage);
        }
    }
}

fn finish(status: &str, inv: &Value, stage: &'static str) -> Result<Value, Outcome> {
    if status == TERMINAL_OK {
        return Ok(inv.get("result").cloned().unwrap_or(Value::Null));
    }
    let error = inv.get("error").and_then(Value::as_str).unwrap_or("no message");
    Err(Outcome::fail(
        "llm_error",
        format!("{stage} call did not complete successfully (status: {status}): {error}"),
        json!({ "stage": stage, "status": status, "inner": inv.get("result").cloned().unwrap_or(Value::Null) }),
    ))
}

/// Best-effort: a console hiccup must not fail the pipeline over what is
/// purely an observability nicety. Returns the cursor to resume from next
/// time — unchanged on any failure.
fn echo_console(host: &dyn Host, action_ref: &str, invocation_id: &str, cursor: i64, stage: &'static str) -> i64 {
    let tail = match host.exec("/builtin/console/tail", &json!({ "action_ref": action_ref, "cursor": cursor })) {
        Ok(c) if c.success => c.result,
        _ => return cursor,
    };
    let next_cursor = tail.get("next_cursor").and_then(Value::as_i64).unwrap_or(cursor);

    let entries = tail.get("entries").and_then(Value::as_array).cloned().unwrap_or_default();
    for entry in entries {
        // The child's console is shared across every concurrent caller
        // (keyed only by action_ref, not by invocation) — only echo the
        // lines this particular call produced.
        if entry.get("invocation_id").and_then(Value::as_str) != Some(invocation_id) {
            continue;
        }
        let level = entry.get("level").and_then(Value::as_str).unwrap_or("info").to_string();
        let message = entry.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        let _ = host.exec(
            "/builtin/console/print",
            &json!({
                "level": level,
                "message": format!("[{stage}] {message}"),
                "data": entry.get("data").cloned().unwrap_or(Value::Null),
            }),
        );
    }

    next_cursor
}

/// Best-effort, same as `solx-ollama`'s own `is_cancelled`: a failure to
/// check (no caller context, host rejection) reads as "not cancelled" —
/// this only ever does anything when `inquire` itself was started via
/// `action_start`, and must not spuriously abort a plain synchronous run.
fn own_invocation_cancelled(host: &dyn Host) -> bool {
    match host.exec("/builtin/action/cancelled", &json!({})) {
        Ok(c) if c.success => c.result.get("cancelled").and_then(Value::as_bool).unwrap_or(false),
        _ => false,
    }
}

pub fn dispatch_failure(p: &Params, stage: &str, err: String) -> Outcome {
    Outcome::fail(
        "dispatch_error",
        format!("could not call {} ({stage}): {err}", p.llm_action_ref),
        json!({ "stage": stage, "action_ref": p.llm_action_ref }),
    )
}

pub fn llm_failure(p: &Params, stage: &str, message: Option<String>, inner: Value) -> Outcome {
    Outcome::fail(
        "llm_error",
        format!(
            "{} call failed ({stage}): {}",
            p.llm_action_ref,
            message.unwrap_or_else(|| "no message".to_string())
        ),
        json!({ "stage": stage, "inner": inner }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_statuses() {
        for s in ["ok", "failed", "cancelled", "timeout", "interrupted"] {
            assert!(is_terminal(s), "{s}");
        }
        for s in ["running", "cancelling"] {
            assert!(!is_terminal(s), "{s}");
        }
    }
}

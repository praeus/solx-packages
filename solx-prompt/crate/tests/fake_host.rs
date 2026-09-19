//! A scripted [`Host`] for the phase tests, shared via `mod fake_host;`.
//!
//! Replays canned `exec` responses per `action_ref` — a separate FIFO queue for
//! each — and records every call made, so the exact sequence of nested calls can
//! be asserted without a network, an Ollama server, a solx-server, or a wasm
//! runtime.
//!
//! **Keying by `action_ref` rather than one flat queue** is what makes a
//! mis-scripted test fail loudly. One llm call drives several distinct
//! `/builtin/action/*` and `/builtin/console/*` calls (see `src/llm.rs`), and
//! both phases reuse the *same* refs — a single flat queue would silently hand a
//! response meant for the intent phase to a call the steps phase made, the
//! moment a test's call-count guess was off by one.
//!
//! **Polls are keyed by invocation id** on top of that. Even without a fan-out,
//! `llm::call` goes `action/start` → `action/cancelled` → `action/poll`, and the
//! two phases' invocations are distinguishable only by id.
//!
//! Anything not queued and not one of the four defaults below **panics**. A
//! FakeHost that quietly invented a response would turn "the pipeline made a
//! call I did not expect" into a passing test.

#![allow(dead_code)] // Each test file uses its own subset of the builders.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use serde_json::{json, Value};

use solx_prompt::host::{Host, HostCall};

pub const LLM_REF: &str = "/packages/solx-ollama/ollama-chat";
pub const ACTION_START: &str = "/builtin/action/start";
pub const ACTION_POLL: &str = "/builtin/action/poll";
pub const ACTION_STOP: &str = "/builtin/action/stop";
pub const ACTION_CANCELLED: &str = "/builtin/action/cancelled";
pub const CONSOLE_PRINT: &str = "/builtin/console/print";
pub const CONSOLE_TAIL: &str = "/builtin/console/tail";
pub const CONSOLE_COPY: &str = "/builtin/console/copy";

pub struct FakeHost {
    pub calls: RefCell<Vec<(String, Value)>>,
    responses: RefCell<HashMap<String, VecDeque<Result<HostCall, String>>>>,
    polls: RefCell<HashMap<String, VecDeque<Value>>>,
}

impl Default for FakeHost {
    fn default() -> Self {
        FakeHost::new()
    }
}

impl FakeHost {
    pub fn new() -> Self {
        FakeHost {
            calls: RefCell::new(Vec::new()),
            responses: RefCell::new(HashMap::new()),
            polls: RefCell::new(HashMap::new()),
        }
    }

    pub fn push_ok(&self, action_ref: &str, result: Value) -> &Self {
        self.responses
            .borrow_mut()
            .entry(action_ref.to_string())
            .or_default()
            .push_back(Ok(HostCall { success: true, message: None, result }));
        self
    }

    /// A call the host accepted but the action reported as failed.
    pub fn push_fail(&self, action_ref: &str, message: &str) -> &Self {
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

    /// A call the host itself refused — a missing action, a malformed ref.
    /// Distinct from [`Self::push_fail`] because `llm::call` branches on which
    /// it got: an `Err` mentioning a long-lived host sends it down the blocking
    /// path, where a `success: false` is just a failed call.
    pub fn push_err(&self, action_ref: &str, message: &str) -> &Self {
        self.responses
            .borrow_mut()
            .entry(action_ref.to_string())
            .or_default()
            .push_back(Err(message.to_string()));
        self
    }

    /// Queue one complete **detached** llm call that finishes on its first poll:
    /// `action/start`, then a poll answering `ok` with `content` as the model's
    /// message.
    ///
    /// `action/cancelled` and the console drain fall through to the defaults, so
    /// a test scripting a phase writes one line per phase.
    pub fn push_llm_call(&self, invocation_id: &str, content: &str) -> &Self {
        self.push_ok(
            ACTION_START,
            json!({
                "invocation_id": invocation_id,
                "action_ref": LLM_REF,
                "console_seq_start": 0,
            }),
        );
        self.push_poll(
            invocation_id,
            json!({
                "invocation_id": invocation_id,
                "action_ref": LLM_REF,
                "status": "ok",
                "result": { "message": { "role": "assistant", "content": content }, "done": true },
                "error": Value::Null,
            }),
        );
        self
    }

    /// Queue one `action/poll` answer for a specific invocation. The last queued
    /// answer repeats, so an extra poll of an already-settled invocation does
    /// not exhaust the script.
    pub fn push_poll(&self, invocation_id: &str, result: Value) -> &Self {
        self.polls
            .borrow_mut()
            .entry(invocation_id.to_string())
            .or_default()
            .push_back(result);
        self
    }

    /// Queue a blocking llm answer, for the path taken when `action/start` is
    /// refused because the host is not long-lived (a bare `solx exec`).
    pub fn push_llm_blocking(&self, content: &str) -> &Self {
        self.push_ok(
            LLM_REF,
            json!({ "message": { "role": "assistant", "content": content }, "done": true }),
        );
        self
    }

    /// Every `action_ref` this host was asked for, in order.
    pub fn refs(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|(r, _)| r.clone()).collect()
    }

    /// How many times `action_ref` was called.
    pub fn count(&self, action_ref: &str) -> usize {
        self.calls.borrow().iter().filter(|(r, _)| r == action_ref).count()
    }

    /// The payloads sent to `action_ref`, in order — for asserting *what* a
    /// phase asked for, not just that it asked.
    pub fn payloads(&self, action_ref: &str) -> Vec<Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|(r, _)| r == action_ref)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// True if `action_ref` was never called. The invariant that this action
    /// persists nothing is asserted with this.
    pub fn never_called(&self, action_ref: &str) -> bool {
        self.count(action_ref) == 0
    }
}

impl Host for FakeHost {
    fn exec(&self, action_ref: &str, payload: &Value) -> Result<HostCall, String> {
        self.calls.borrow_mut().push((action_ref.to_string(), payload.clone()));

        if action_ref == ACTION_POLL {
            if let Some(id) = payload.get("invocation_id").and_then(Value::as_str) {
                if let Some(queue) = self.polls.borrow_mut().get_mut(id) {
                    // The last queued response repeats, so an extra poll of an
                    // already-answered invocation does not exhaust the script.
                    let value =
                        if queue.len() > 1 { queue.pop_front() } else { queue.front().cloned() };
                    if let Some(value) = value {
                        return Ok(HostCall { success: true, message: None, result: value });
                    }
                }
            }
        }

        let queued =
            self.responses.borrow_mut().get_mut(action_ref).and_then(VecDeque::pop_front);
        if let Some(queued) = queued {
            return queued;
        }

        let default = match action_ref {
            CONSOLE_PRINT => json!({ "seq": 1 }),
            CONSOLE_TAIL => json!({ "entries": [], "next_cursor": 0, "first_seq": 0, "dropped": 0 }),
            // `drain_console` calls this once per tracked invocation on every
            // drain, unconditionally - cheap when there is nothing new, which
            // this default mirrors: nothing copied, and the cursor holds rather
            // than advancing.
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

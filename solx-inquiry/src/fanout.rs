//! Run several llm calls **at the same time** and drive them as one.
//!
//! [`crate::llm::call`] starts one detached invocation and long-polls it to
//! completion. That shape cannot be reused N times for N inquiries: each
//! `action-poll` would block on its own child, so three calls would finish in
//! the sum of their durations rather than the longest of them, and the whole
//! point of fanning out would be lost.
//!
//! So this module inverts the loop. Every job is started first
//! (`/builtin/action/start` returns the moment the task is spawned — see
//! `solx-actions::start_invocation`, a plain `tokio::spawn`), and only then is
//! anything waited on. Each iteration:
//!
//! 1. **Cancellation first**, so a cancelled fan-out does not sit out a whole
//!    wait — and on cancel *every* outstanding child is stopped, not just the
//!    one that happened to be polled.
//! 2. **One `console/tail`**, which covers every child at once: they all run
//!    the same `llm_action_ref`, and a console is keyed by `action_ref` alone.
//!    This is also the loop's pacing, because `tail` waits when there is
//!    nothing to read.
//! 3. **A non-blocking poll of each outstanding child** (`action-poll` with no
//!    `wait_secs` returns immediately), so no child's completion is held up
//!    behind another's.
//!
//! Step 2 has a trap that step 3 cannot see: the shared console also carries
//! lines from *unrelated* concurrent callers of the same chat action, and
//! `tail` returns the instant any entry exists. On a busy console it therefore
//! never waits, and the loop would spin. [`SPIN_GUARD_WAIT_SECS`] covers that
//! case, and only that case.
//!
//! A job that fails fails **alone**. A fan-out that sank every inquiry because
//! one model call errored would be strictly worse than running them one at a
//! time, so a failure is recorded against its own job and the rest run on.
//!
//! This module is also where an inquiry's **lifecycle** is narrated to the
//! console ([`crate::console`]), and it has to be: [`run`] returns only once
//! every job is terminal, so nothing upstream can observe an individual start
//! or completion at the moment it happens. Between `multi.rs`'s pre-fan-out
//! `:hits` print and its post-fan-out `:result` print sits the longest silent
//! stretch of a researched run, and it is exactly the window in which a UI
//! wants to say "2 inquiries running".

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::console;
use crate::host::{split_ref, Host, Outcome};
use crate::llm::{
    drain_console, is_terminal, own_invocation_cancelled, ACTION_POLL_REF, ACTION_START_REF,
    ACTION_STOP_REF, LONG_LIVED_HOST_MARKER, POLL_WAIT_SECS,
};
use crate::params::Params;

/// How long to wait on one child when the shared console is busy with other
/// callers' output. Short, because it is only ever reached when `tail`
/// returned immediately with nothing of ours — the loop is otherwise paced by
/// `tail` itself.
const SPIN_GUARD_WAIT_SECS: u64 = 2;

/// Status word reported for an inquiry that never reached the model at all —
/// its `action-start` or `action-poll` was refused. Not one of `llm`'s
/// terminal statuses, because the invocation store never had an opinion about
/// it, but terminal from this module's point of view all the same.
const STATUS_DISPATCH_ERROR: &str = "dispatch_error";

/// One llm call to run: the payload, the label its console lines and errors
/// are tagged with, and enough about the inquiry behind it to narrate it.
///
/// `index` is the **inquiry number**, carried explicitly rather than taken
/// from the job's position: `multi.rs` builds jobs from `prepared`, which is
/// shorter than `intent.inquiries` whenever a search failed, so position and
/// inquiry number stop agreeing the moment anything has gone wrong.
///
/// `meta` is opaque here — `{kind, q}`, built by the caller that owns the
/// domain knowledge and echoed verbatim into this module's console events.
/// That keeps the fan-out ignorant of inquiry semantics while still letting it
/// be the thing that reports them, and it keeps the console vocabulary in one
/// place (see [`crate::console`]'s module doc) rather than splitting it across
/// a callback.
pub struct Job {
    pub index: usize,
    pub label: String,
    pub payload: Value,
    pub meta: Value,
}

/// What the fan-out produced, one entry per job, in the order the jobs were
/// given. `Err` is that job's own failure; the others may still be `Ok`.
pub type JobResults = Vec<Result<Value, Outcome>>;

struct Pending {
    /// Position in `jobs`, and so the slot in `results` this job writes to.
    slot: usize,
    /// The inquiry number this job is running — what the console reports.
    index: usize,
    invocation_id: String,
    label: String,
}

/// Run every job. `Err` means the whole fan-out was abandoned — only
/// cancellation does that; every other failure lands on one job.
pub fn run(host: &dyn Host, p: &Params, jobs: Vec<Job>) -> Result<JobResults, Outcome> {
    if jobs.is_empty() {
        return Ok(Vec::new());
    }

    // A malformed `llm_action_ref` cannot be started detached at all
    // (`action-start` takes a path/name pair, not a joined ref), the same
    // fallback `llm::call` makes for the same reason.
    let Some((path, name)) = split_ref(&p.llm_action_ref) else {
        announce_degraded(host, jobs.len(), "malformed_action_ref");
        return Ok(run_sequentially(host, p, &jobs));
    };

    let total = jobs.len();
    let mut results: JobResults = jobs.iter().map(|_| Ok(Value::Null)).collect();
    let mut pending: Vec<Pending> = Vec::new();
    let mut cursor: Option<i64> = None;

    for (slot, job) in jobs.iter().enumerate() {
        let start = host.exec(
            ACTION_START_REF,
            &json!({ "path": path, "name": name, "params": job.payload }),
        );
        let (ok, message) = match start {
            Ok(c) if c.success => (Some(c.result), String::new()),
            Ok(c) => (None, c.message.unwrap_or_default()),
            Err(e) => (None, e),
        };

        let Some(started) = ok else {
            // `is_long_lived_host` is a process-wide flag, so this refusal can
            // only ever come back on the *first* attempt: if job 0 started,
            // job 1 cannot be refused for this reason. Restricting the
            // fallback to `slot == 0` is what makes it safe to re-run the
            // whole batch blocking - at that point nothing has been started,
            // so nothing can be run twice.
            if slot == 0 && message.contains(LONG_LIVED_HOST_MARKER) {
                host.log(
                    "solx-inquiry: host cannot detach calls; running inquiries sequentially",
                );
                announce_degraded(host, total, "host_cannot_detach");
                return Ok(run_sequentially(host, p, &jobs));
            }
            // Terminal for this inquiry even though it never reached the
            // model: without it a progress UI would leave the row spinning
            // for the rest of the run. The human-readable `:error` line for
            // the same failure is `multi.rs`'s to print, once it zips the
            // results - printing it here too would double it in the CLI.
            announce_finished(host, job.index, STATUS_DISPATCH_ERROR, false);
            results[slot] = Err(Outcome::fail(
                "dispatch_error",
                format!("action-start failed ({}): {message}", job.label),
                json!({ "stage": job.label, "action_ref": p.llm_action_ref }),
            ));
            continue;
        };

        let Some(invocation_id) = started.get("invocation_id").and_then(Value::as_str) else {
            announce_finished(host, job.index, STATUS_DISPATCH_ERROR, false);
            results[slot] = Err(Outcome::fail(
                "dispatch_error",
                format!("action-start returned no invocation_id ({})", job.label),
                json!({ "stage": job.label }),
            ));
            continue;
        };

        // The earliest start point across every child, so no child's opening
        // lines are skipped by a cursor set from a later one.
        let seq = started.get("console_seq_start").and_then(Value::as_i64).unwrap_or(0);
        cursor = Some(cursor.map_or(seq, |c: i64| c.min(seq)));

        pending.push(Pending {
            slot,
            index: job.index,
            invocation_id: invocation_id.to_string(),
            label: job.label.clone(),
        });
        // After the push, so an inquiry whose start was refused never reports
        // as started.
        announce_started(host, job.index, total, &job.meta);
    }

    // One event carrying the denominator, so a consumer that joined mid-run -
    // or whose earlier entries were evicted from this shared console - can
    // still size the fan-out it is watching.
    console::print_ev(
        host,
        &console::phase_tag(console::PHASE_FANOUT),
        &format!("{} running", plural_inquiries(pending.len())),
        console::ev(
            console::EV_FANOUT_STARTED,
            json!({ "n": pending.len(), "mode": "parallel" }),
        ),
        Value::Null,
    );

    let mut cursor = cursor.unwrap_or(0);
    // Persisted across iterations, one entry per child, so `drain_console`'s
    // `console/copy` calls each resume where their own last call left off -
    // independent of `cursor`, which only ever tracks the shared tail.
    let mut copy_cursors: HashMap<String, i64> = HashMap::new();

    while !pending.is_empty() {
        if own_invocation_cancelled(host) {
            // Otherwise this path is console-silent, and a UI watching the
            // feed cannot tell a cancelled run from a stalled one.
            console::warn_ev(
                host,
                &console::phase_step_tag(console::PHASE_RUN, console::STEP_CANCELLED),
                &format!("cancelled with {} still running", plural_inquiries(pending.len())),
                console::ev(console::EV_RUN_CANCELLED, json!({ "stopped": pending.len() })),
                Value::Null,
            );
            stop_all(host, &pending);
            return Err(Outcome::fail(
                "cancelled",
                "multi_inquire was cancelled while its inquiries were running",
                json!({
                    "stage": "inquiry",
                    "stopped": pending.iter().map(|j| j.invocation_id.clone()).collect::<Vec<_>>(),
                }),
            ));
        }

        let labels: Vec<(String, String)> = pending
            .iter()
            .map(|j| (j.invocation_id.clone(), j.label.clone()))
            .collect();
        let echo =
            drain_console(host, &p.llm_action_ref, cursor, &labels, &mut copy_cursors, Some(POLL_WAIT_SECS));
        cursor = echo.cursor;

        let mut finished_any = false;
        let mut still: Vec<Pending> = Vec::new();
        for job in pending {
            match poll_once(host, &job) {
                Some(outcome) => {
                    results[job.slot] = outcome;
                    finished_any = true;
                }
                None => still.push(job),
            }
        }
        pending = still;

        // Whenever the tail did not actually pace this iteration and nothing
        // moved. Two ways that happens, and both would otherwise turn this into
        // a hot loop of polls for the whole action timeout:
        //
        // * it returned instantly because entries exist that are somebody
        //   else's - the console is shared, keyed by `action_ref` alone;
        // * it *failed*, which returns instantly and keeps doing so.
        //
        // Waiting on one child is enough - every other child is polled again
        // the moment this returns.
        let tail_paced_us = echo.paced && !(echo.saw_entries && echo.copied == 0);
        if !pending.is_empty() && !finished_any && !tail_paced_us {
            let _ = host.exec(
                ACTION_POLL_REF,
                &json!({ "invocation_id": pending[0].invocation_id, "wait_secs": SPIN_GUARD_WAIT_SECS }),
            );
        }
    }

    Ok(results)
}

/// `Some` once this job has reached a terminal status (or its poll failed);
/// `None` while it is still running.
fn poll_once(host: &dyn Host, job: &Pending) -> Option<Result<Value, Outcome>> {
    let poll = match host.exec(ACTION_POLL_REF, &json!({ "invocation_id": job.invocation_id })) {
        Ok(c) if c.success => c.result,
        Ok(c) => {
            announce_finished(host, job.index, STATUS_DISPATCH_ERROR, false);
            return Some(Err(Outcome::fail(
                "dispatch_error",
                format!("action-poll failed ({}): {}", job.label, c.message.unwrap_or_default()),
                json!({ "stage": job.label, "invocation_id": job.invocation_id }),
            )));
        }
        Err(e) => {
            announce_finished(host, job.index, STATUS_DISPATCH_ERROR, false);
            return Some(Err(Outcome::fail(
                "dispatch_error",
                format!("action-poll failed ({}): {e}", job.label),
                json!({ "stage": job.label, "invocation_id": job.invocation_id }),
            )));
        }
    };

    let status = poll.get("status").and_then(Value::as_str).unwrap_or("");
    if !is_terminal(status) {
        return None;
    }
    announce_finished(host, job.index, status, status == "ok");
    Some(crate::llm::finish(status, &poll, &job.label))
}

/// Best-effort, and deliberately every child rather than the first failure:
/// leaving a detached model call running after the pipeline that started it
/// has been cancelled is exactly what cooperative cancellation exists to
/// prevent.
fn stop_all(host: &dyn Host, pending: &[Pending]) {
    for job in pending {
        let _ = host.exec(ACTION_STOP_REF, &json!({ "invocation_id": job.invocation_id }));
    }
}

/// The degraded path: no detached invocations, so the jobs run one after
/// another. Correct, just not concurrent — and without live console echo or
/// cooperative cancellation, the same trade `inquire` already makes under a
/// bare `solx exec`.
///
/// Only ever reached before anything has been started detached, so no job can
/// be run twice - see the `slot == 0` guard at the one call site that can
/// reach it after a start attempt.
///
/// Still narrates each inquiry, so a progress UI shows work advancing one at a
/// time rather than a frozen strip for the whole run.
fn run_sequentially(host: &dyn Host, p: &Params, jobs: &[Job]) -> JobResults {
    let total = jobs.len();
    jobs.iter()
        .map(|job| {
            announce_started(host, job.index, total, &job.meta);
            let outcome = crate::llm::call_blocking(host, p, &job.payload, &job.label);
            let ok = outcome.is_ok();
            announce_finished(host, job.index, if ok { "ok" } else { "failed" }, ok);
            outcome
        })
        .collect()
}

/// Report a per-job failure to the console without failing the run. Kept here
/// so the fan-out's error vocabulary and its console vocabulary stay together.
///
/// A failed inquiry therefore produces **two** events: the lifecycle
/// `inquiry.finished { ok: false }` above, and this diagnosis. They answer
/// different questions, either may be the only one a consumer receives, and a
/// consumer that never moves an inquiry backwards through its phases handles
/// the pair in any arrival order.
pub fn log_job_failure(host: &dyn Host, index: usize, outcome: &Outcome) {
    let stage = outcome
        .output
        .get("stage")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reason = outcome
        .output
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("inquiry_error")
        .to_string();
    console::warn_ev(
        host,
        &console::inquiry_step_tag(index, console::STEP_ERROR),
        outcome.message.as_deref().unwrap_or("inquiry failed"),
        console::ev(
            console::EV_INQUIRY_FAILED,
            json!({ "i": index, "stage": stage, "reason": reason }),
        ),
        outcome.output.clone(),
    );
}

/// `[multi_inquire:inquiry:<i>:start]` — one inquiry has reached the model.
fn announce_started(host: &dyn Host, index: usize, n: usize, meta: &Value) {
    let question = meta.get("q").and_then(Value::as_str);
    let message = match question {
        Some(q) => format!("started: {q}"),
        None => "started".to_string(),
    };
    let mut fields = json!({ "i": index, "n": n });
    if let (Some(into), Some(from)) = (fields.as_object_mut(), meta.as_object()) {
        for (key, value) in from {
            into.insert(key.clone(), value.clone());
        }
    }
    console::print_ev(
        host,
        &console::inquiry_step_tag(index, console::STEP_START),
        &message,
        console::ev(console::EV_INQUIRY_STARTED, fields),
        Value::Null,
    );
}

/// `[multi_inquire:inquiry:<i>:done]` — one inquiry has stopped running,
/// however it stopped.
fn announce_finished(host: &dyn Host, index: usize, status: &str, ok: bool) {
    console::print_ev(
        host,
        &console::inquiry_step_tag(index, console::STEP_DONE),
        &format!("finished ({status})"),
        console::ev(
            console::EV_INQUIRY_FINISHED,
            json!({ "i": index, "status": status, "ok": ok }),
        ),
        Value::Null,
    );
}

/// `[multi_inquire:fanout]` — the inquiries are running one at a time. A
/// user-visible performance fact that was previously only reachable through
/// the WIT logger, which never lands on the tagged console.
fn announce_degraded(host: &dyn Host, n: usize, reason: &str) {
    console::warn_ev(
        host,
        &console::phase_step_tag(console::PHASE_FANOUT, console::STEP_DEGRADED),
        &format!("running {} one at a time", plural_inquiries(n)),
        console::ev(
            console::EV_FANOUT_DEGRADED,
            json!({ "n": n, "reason": reason, "mode": "sequential" }),
        ),
        Value::Null,
    );
}

fn plural_inquiries(n: usize) -> String {
    if n == 1 {
        "1 inquiry".to_string()
    } else {
        format!("{n} inquiries")
    }
}

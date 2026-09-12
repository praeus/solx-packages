//! Run several llm calls **at the same time** and drive them as one.
//!
//! [`crate::llm::call`] starts one detached invocation and long-polls it to
//! completion. That shape cannot be reused N times for N inquiries: each
//! `action_poll` would block on its own child, so three calls would finish in
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
//! 3. **A non-blocking poll of each outstanding child** (`action_poll` with no
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

/// One llm call to run: the payload, and the label its console lines and
/// errors are tagged with.
pub struct Job {
    pub label: String,
    pub payload: Value,
}

/// What the fan-out produced, one entry per job, in the order the jobs were
/// given. `Err` is that job's own failure; the others may still be `Ok`.
pub type JobResults = Vec<Result<Value, Outcome>>;

struct Pending {
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
    // (`action_start` takes a path/name pair, not a joined ref), the same
    // fallback `llm::call` makes for the same reason.
    let Some((path, name)) = split_ref(&p.llm_action_ref) else {
        return Ok(run_sequentially(host, p, &jobs));
    };

    let mut results: JobResults = jobs.iter().map(|_| Ok(Value::Null)).collect();
    let mut pending: Vec<Pending> = Vec::new();
    let mut cursor: Option<i64> = None;

    for (index, job) in jobs.iter().enumerate() {
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
            // fallback to `index == 0` is what makes it safe to re-run the
            // whole batch blocking - at that point nothing has been started,
            // so nothing can be run twice.
            if index == 0 && message.contains(LONG_LIVED_HOST_MARKER) {
                host.log(
                    "solx-inquiry: host cannot detach calls; running inquiries sequentially",
                );
                return Ok(run_sequentially(host, p, &jobs));
            }
            results[index] = Err(Outcome::fail(
                "dispatch_error",
                format!("action_start failed ({}): {message}", job.label),
                json!({ "stage": job.label, "action_ref": p.llm_action_ref }),
            ));
            continue;
        };

        let Some(invocation_id) = started.get("invocation_id").and_then(Value::as_str) else {
            results[index] = Err(Outcome::fail(
                "dispatch_error",
                format!("action_start returned no invocation_id ({})", job.label),
                json!({ "stage": job.label }),
            ));
            continue;
        };

        // The earliest start point across every child, so no child's opening
        // lines are skipped by a cursor set from a later one.
        let seq = started.get("console_seq_start").and_then(Value::as_i64).unwrap_or(0);
        cursor = Some(cursor.map_or(seq, |c: i64| c.min(seq)));

        pending.push(Pending {
            index,
            invocation_id: invocation_id.to_string(),
            label: job.label.clone(),
        });
    }

    let mut cursor = cursor.unwrap_or(0);
    // Persisted across iterations, one entry per child, so `drain_console`'s
    // `console/copy` calls each resume where their own last call left off -
    // independent of `cursor`, which only ever tracks the shared tail.
    let mut copy_cursors: HashMap<String, i64> = HashMap::new();

    while !pending.is_empty() {
        if own_invocation_cancelled(host) {
            stop_all(host, &pending);
            return Err(Outcome::fail(
                "cancelled",
                "instruct was cancelled while its inquiries were running",
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
                    results[job.index] = outcome;
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
            return Some(Err(Outcome::fail(
                "dispatch_error",
                format!("action_poll failed ({}): {}", job.label, c.message.unwrap_or_default()),
                json!({ "stage": job.label, "invocation_id": job.invocation_id }),
            )))
        }
        Err(e) => {
            return Some(Err(Outcome::fail(
                "dispatch_error",
                format!("action_poll failed ({}): {e}", job.label),
                json!({ "stage": job.label, "invocation_id": job.invocation_id }),
            )))
        }
    };

    let status = poll.get("status").and_then(Value::as_str).unwrap_or("");
    if !is_terminal(status) {
        return None;
    }
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
/// be run twice - see the `index == 0` guard at the one call site that can
/// reach it after a start attempt.
fn run_sequentially(host: &dyn Host, p: &Params, jobs: &[Job]) -> JobResults {
    jobs.iter()
        .map(|job| crate::llm::call_blocking(host, p, &job.payload, &job.label))
        .collect()
}

/// Report a per-job failure to the console without failing the run. Kept here
/// so the fan-out's error vocabulary and its console vocabulary stay together.
pub fn log_job_failure(host: &dyn Host, index: usize, outcome: &Outcome) {
    console::warn(
        host,
        &console::inquiry_step_tag(index, console::STEP_ERROR),
        outcome.message.as_deref().unwrap_or("inquiry failed"),
        outcome.output.clone(),
    );
}

//! The child's side of a solx-core **background command**: one long-lived
//! process serving many action calls over its stdin and stdout, instead of
//! one process per call (solx-core's `docs/background-commands-plan.md`).
//!
//! A package declares Command actions with an `action_config.background`
//! object: one with `backgroundOperation: "start"` whose command runs this
//! binary, optionally one with `"stop"`, and one per operation. solx-core
//! starts the process on first use, sets [`ENV_BACKGROUND`] on it, and
//! forwards each operation's params here as a request. [`serve`] runs the
//! package's handler for each and sends back what it returns:
//!
//! ```no_run
//! # async fn run() -> std::io::Result<()> {
//! use serde_json::json;
//! solx_package_lib::init("solx-graph");
//! solx_package_lib::background::serve(|call| async move {
//!     match call.op.as_str() {
//!         "query" => Ok(json!({ "rows": [] })),
//!         op => Err(format!("unknown operation '{op}'")),
//!     }
//! })
//! .await
//! # }
//! ```
//!
//! # Stdout belongs to the protocol
//!
//! Every line on stdout must be a protocol message. **Anything else a
//! package prints there — `println!`, [`crate::print_json`], a dependency
//! writing to stdout — makes solx-core treat the stream as corrupt and kill
//! the process.** Log with [`crate::info`]/[`crate::warn`]/[`crate::error`]
//! (or `eprintln!`, which goes to the start action's `background.log`).
//!
//! # Calling the host
//!
//! [`Call::host`] makes requests the other way: run another action
//! ([`Host::exec`]) or write to the console ([`Host::print`]). They run as
//! the package's *start* action, so they get its `action_scopes` and no
//! more, whichever operation woke the process. A background gets no
//! `SOLX_SERVER_URL`/`SOLX_SERVER_TOKEN`; this is the way back in.
//!
//! # Logging
//!
//! A background has none of the per-invocation console env vars a one-shot
//! command gets. While [`serve`] runs, the crate's logging calls send a
//! `console.print` request instead, to the console of the call being served
//! (or the start action's, outside one), and still mirror to stderr and
//! `SOL_LOG_DIR`. Logging from a task the handler spawns itself loses track
//! of the call; log through [`Call::host`] there.
//!
//! # One at a time
//!
//! [`serve`] runs handlers concurrently. [`serve_one_at_a_time`] runs them
//! in arrival order, one after another — the simple choice for a package
//! whose operations all write to something single-writer, like an embedded
//! database. A package with concurrent reads and serialized writes keeps
//! [`serve`] and holds its own `tokio::sync::Mutex` around the writes.
//!
//! Either way, the reader keeps going while a handler runs, so a handler
//! waiting on a host call never blocks the response it's waiting for.
//!
//! # Shutdown
//!
//! On `shutdown`, or when the host closes stdin, `serve` refuses new
//! requests, lets the handlers already running finish, and returns. solx-core
//! kills the process after `stop_grace_secs` regardless.
//!
//! The wire types mirror solx-core's `solx-surface/src/background.rs`; this
//! crate doesn't depend on core, so a change there is a change here too.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Notify};

/// Set by solx-core on a background's process, to
/// `<backgroundNamespace>/<backgroundName>`.
pub const ENV_BACKGROUND: &str = "SOLX_BACKGROUND";

/// Host → background: stop serving and exit.
pub const OP_SHUTDOWN: &str = "shutdown";
/// Background → host: run an action, `{path, name, params}`.
pub const OP_EXEC: &str = "exec";
/// Background → host: write to a console, `{level, message, data?, invocation_id?}`.
pub const OP_CONSOLE_PRINT: &str = "console.print";

/// Whether this process was started as a background — for a binary that
/// also runs as a one-shot command ([`crate::stdin_params`]) otherwise.
pub fn is_background() -> bool {
    std::env::var_os(ENV_BACKGROUND).is_some_and(|v| !v.is_empty())
}

/// One operation forwarded from the host.
pub struct Call {
    /// The operation's `backgroundOperation`.
    pub op: String,
    /// The action call's params, as given.
    pub params: Value,
    /// The host invocation this call belongs to.
    pub invocation_id: Option<String>,
    host: Host,
}

impl Call {
    /// The host, with requests tagged as part of this call: they run under
    /// its chain, and print to its console.
    pub fn host(&self) -> &Host {
        &self.host
    }
}

/// How [`serve_with`] runs handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Concurrency {
    /// Each request on its own task, as it arrives.
    #[default]
    Concurrent,
    /// One after another, in arrival order.
    OneAtATime,
}

/// Serve requests from stdin until `shutdown` or end of input, running
/// `handler` concurrently for each. Its `Err` is the action's error message.
pub async fn serve<H, Fut>(handler: H) -> io::Result<()>
where
    H: Fn(Call) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    serve_with(tokio::io::stdin(), tokio::io::stdout(), Concurrency::Concurrent, handler).await
}

/// [`serve`], with one handler running at a time, in arrival order.
pub async fn serve_one_at_a_time<H, Fut>(handler: H) -> io::Result<()>
where
    H: Fn(Call) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    serve_with(tokio::io::stdin(), tokio::io::stdout(), Concurrency::OneAtATime, handler).await
}

/// [`serve`] over any pipes — for a package's own tests, in-memory.
pub async fn serve_with<R, W, H, Fut>(
    reader: R,
    writer: W,
    concurrency: Concurrency,
    handler: H,
) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    H: Fn(Call) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    let (lines_tx, lines_rx) = mpsc::unbounded_channel();
    let (finish, finished) = oneshot::channel();
    let writer_task = tokio::spawn(write_lines(writer, lines_rx, finished));
    let host = Host {
        link: Arc::new(Link {
            lines: lines_tx,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        }),
        invocation_id: None,
    };
    let _active = ActiveHost::set(host.clone());

    let handler = Arc::new(handler);
    let inflight = Arc::new(Inflight::default());
    let queue = match concurrency {
        Concurrency::Concurrent => None,
        Concurrency::OneAtATime => {
            let (tx, mut rx) = mpsc::unbounded_channel::<(u64, Call)>();
            let handler = handler.clone();
            let inflight = inflight.clone();
            tokio::spawn(async move {
                while let Some((id, call)) = rx.recv().await {
                    run_one(&*handler, id, call).await;
                    inflight.done();
                }
            });
            Some(tx)
        }
    };

    let mut lines = BufReader::new(reader).lines();
    let mut draining = false;
    let mut eof = false;
    let result = loop {
        if draining && inflight.is_empty() {
            break Ok(());
        }
        // Still reading while draining after `shutdown`: the calls being
        // waited out may be waiting on host answers themselves.
        let line = tokio::select! {
            line = lines.next_line(), if !eof => line,
            _ = inflight.changed.notified(), if draining => continue,
        };
        let line = match line {
            Ok(Some(line)) => line,
            // The host is gone: nobody will answer a host call again.
            Ok(None) => {
                host.link.close();
                eof = true;
                draining = true;
                continue;
            }
            Err(e) => break Err(e),
        };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("solx background: ignoring a line that isn't JSON ({e})");
                continue;
            }
        };
        match message["kind"].as_str() {
            Some("response") => host.link.complete(&message),
            Some("request") => {
                let id = message["id"].as_u64().unwrap_or(0);
                let op = message["op"].as_str().unwrap_or_default().to_string();
                if op == OP_SHUTDOWN {
                    host.link.send(response(id, Ok(Value::Null)));
                    draining = true;
                    continue;
                }
                if draining {
                    host.link.send(response(id, Err("the background is shutting down".into())));
                    continue;
                }
                let invocation_id = message["invocation_id"].as_str().map(str::to_string);
                let call = Call {
                    op,
                    params: message.get("params").cloned().unwrap_or(Value::Null),
                    host: Host {
                        link: host.link.clone(),
                        invocation_id: invocation_id.clone(),
                    },
                    invocation_id,
                };
                inflight.start();
                match &queue {
                    Some(queue) => {
                        let _ = queue.send((id, call));
                    }
                    None => {
                        let handler = handler.clone();
                        let inflight = inflight.clone();
                        tokio::spawn(async move {
                            run_one(&*handler, id, call).await;
                            inflight.done();
                        });
                    }
                }
            }
            _ => eprintln!("solx background: ignoring a message with no kind"),
        }
    };

    drop(queue);
    drop(_active);
    // Not waiting for every sender to drop: a task a handler left behind
    // may still hold a `Host`.
    let _ = finish.send(());
    let _ = writer_task.await;
    result
}

/// Run `handler` for one call and send its response. On its own task, so a
/// panic becomes an error response instead of a call the host waits on
/// until it times out.
async fn run_one<H, Fut>(handler: &H, id: u64, call: Call)
where
    H: Fn(Call) -> Fut,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    let link = call.host.link.clone();
    let invocation_id = call.invocation_id.clone();
    let fut = CURRENT_INVOCATION.scope(invocation_id, handler(call));
    let outcome = match tokio::spawn(fut).await {
        Ok(outcome) => outcome,
        Err(e) if e.is_panic() => Err("the background's handler panicked".into()),
        Err(e) => Err(format!("the background's handler didn't finish: {e}")),
    };
    link.send(response(id, outcome));
}

fn response(id: u64, outcome: Result<Value, String>) -> Value {
    match outcome {
        Ok(result) => json!({ "kind": "response", "id": id, "ok": true, "result": result }),
        Err(error) => json!({ "kind": "response", "id": id, "ok": false, "error": error }),
    }
}

/// Owns the writer until `finish`, then writes what's already queued and
/// stops.
async fn write_lines<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut lines: mpsc::UnboundedReceiver<String>,
    mut finish: oneshot::Receiver<()>,
) {
    let mut finishing = false;
    loop {
        let line = if finishing {
            lines.recv().await
        } else {
            tokio::select! {
                biased;
                line = lines.recv() => line,
                _ = &mut finish => {
                    finishing = true;
                    lines.close();
                    continue;
                }
            }
        };
        let Some(mut line) = line else { break };
        line.push('\n');
        if writer.write_all(line.as_bytes()).await.is_err() || writer.flush().await.is_err() {
            break;
        }
    }
}

/// How many calls are queued or running, so a shutdown can wait them out.
#[derive(Default)]
struct Inflight {
    count: AtomicUsize,
    changed: Notify,
}

impl Inflight {
    fn start(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    fn done(&self) {
        self.count.fetch_sub(1, Ordering::SeqCst);
        self.changed.notify_one();
    }

    fn is_empty(&self) -> bool {
        self.count.load(Ordering::SeqCst) == 0
    }
}

/// The pipes back to the host, shared by every [`Host`].
struct Link {
    lines: mpsc::UnboundedSender<String>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    /// Set once the host closed stdin.
    closed: AtomicBool,
}

impl Link {
    fn send(&self, message: Value) {
        let _ = self.lines.send(message.to_string());
    }

    fn complete(&self, message: &Value) {
        let Some(id) = message["id"].as_u64() else { return };
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(message.clone());
        }
    }

    /// Fails every waiting [`Host::call`] (dropping its sender), and every
    /// one from here on.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.pending.lock().unwrap().clear();
    }

    fn request(&self, op: &str, params: Value, invocation_id: Option<&str>) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut message = json!({ "kind": "request", "id": id, "op": op, "params": params });
        if let Some(invocation_id) = invocation_id {
            message["invocation_id"] = json!(invocation_id);
        }
        message
    }
}

/// Requests from the background to the host. Cheap to clone.
#[derive(Clone)]
pub struct Host {
    link: Arc<Link>,
    invocation_id: Option<String>,
}

impl Host {
    /// Send `op` to the host and wait for its answer: the response's
    /// `result`, or its `error`.
    pub async fn call(&self, op: &str, params: Value) -> Result<Value, String> {
        let message = self.link.request(op, params, self.invocation_id.as_deref());
        let id = message["id"].as_u64().unwrap_or_default();
        let (tx, rx) = oneshot::channel();
        self.link.pending.lock().unwrap().insert(id, tx);
        // After registering, so a `close` racing this drops `tx` either way.
        if self.link.closed.load(Ordering::SeqCst) {
            self.link.pending.lock().unwrap().remove(&id);
        }
        self.link.send(message);
        let response = rx
            .await
            .map_err(|_| format!("host op '{op}': the host stopped answering"))?;
        if response["ok"].as_bool() == Some(true) {
            Ok(response.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(response["error"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("host op '{op}' failed")))
        }
    }

    /// Run the action at `path`/`name` as the package's start action, and
    /// return its `result`.
    pub async fn exec(&self, path: &str, name: &str, params: Value) -> Result<Value, String> {
        let exec = self
            .call(OP_EXEC, json!({ "path": path, "name": name, "params": params }))
            .await?;
        if exec["success"].as_bool() == Some(false) {
            return Err(exec["message"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("{path}/{name} failed")));
        }
        Ok(exec.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Write to the console of the call this handle belongs to.
    pub async fn print(&self, level: &str, message: &str, data: Option<Value>) -> Result<(), String> {
        self.call(OP_CONSOLE_PRINT, print_params(level, message, data, None))
            .await
            .map(|_| ())
    }

    /// [`Host::print`] without waiting for the answer: what logging uses.
    fn print_and_forget(&self, level: &str, message: &str, data: Option<Value>, invocation_id: Option<&str>) {
        let invocation_id = invocation_id.or(self.invocation_id.as_deref());
        // Nobody waits on it, so the host's answer is dropped on arrival.
        self.link.send(self.link.request(
            OP_CONSOLE_PRINT,
            print_params(level, message, data, invocation_id),
            invocation_id,
        ));
    }
}

fn print_params(level: &str, message: &str, data: Option<Value>, invocation_id: Option<&str>) -> Value {
    let mut params = json!({ "level": level, "message": message });
    if let Some(data) = data {
        params["data"] = data;
    }
    if let Some(invocation_id) = invocation_id {
        params["invocation_id"] = json!(invocation_id);
    }
    params
}

tokio::task_local! {
    /// The invocation the handler on this task is serving, for logging.
    static CURRENT_INVOCATION: Option<String>;
}

/// The host [`serve`] is talking to, while it runs: where logging goes.
static ACTIVE: Mutex<Option<Host>> = Mutex::new(None);

struct ActiveHost;

impl ActiveHost {
    fn set(host: Host) -> Self {
        *ACTIVE.lock().unwrap() = Some(host);
        ActiveHost
    }
}

impl Drop for ActiveHost {
    fn drop(&mut self) {
        *ACTIVE.lock().unwrap() = None;
    }
}

/// Route a log line to the host's console, if [`serve`] is running. Returns
/// whether it did, so the caller skips the HTTP loopback.
pub(crate) fn log_to_host(level: &str, message: &str, data: Option<Value>) -> bool {
    let Some(host) = ACTIVE.lock().unwrap().clone() else {
        return false;
    };
    let invocation_id = CURRENT_INVOCATION.try_with(|id| id.clone()).ok().flatten();
    host.print_and_forget(level, message, data, invocation_id.as_deref());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

    /// The host's end of the pipes.
    struct FakeHost {
        to_bg: DuplexStream,
        from_bg: Lines<BufReader<DuplexStream>>,
    }

    impl FakeHost {
        async fn send(&mut self, message: Value) {
            let line = format!("{message}\n");
            self.to_bg.write_all(line.as_bytes()).await.unwrap();
        }

        async fn request(&mut self, id: u64, op: &str, params: Value) {
            self.send(json!({"kind": "request", "id": id, "op": op, "params": params, "invocation_id": format!("inv-{id}")}))
                .await;
        }

        async fn next(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(5), self.from_bg.next_line())
                .await
                .expect("the background should have written a line")
                .unwrap()
                .expect("the background closed stdout");
            serde_json::from_str(&line).unwrap()
        }
    }

    fn start<H, Fut>(concurrency: Concurrency, handler: H) -> (FakeHost, tokio::task::JoinHandle<io::Result<()>>)
    where
        H: Fn(Call) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        let (to_bg, bg_in) = tokio::io::duplex(64 * 1024);
        let (bg_out, from_bg) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(serve_with(bg_in, bg_out, concurrency, handler));
        (
            FakeHost {
                to_bg,
                from_bg: BufReader::new(from_bg).lines(),
            },
            task,
        )
    }

    /// `sleep` waits `ms` then answers `value`; `exec` calls the host; `fail` errs.
    async fn handler(call: Call) -> Result<Value, String> {
        match call.op.as_str() {
            "sleep" => {
                tokio::time::sleep(Duration::from_millis(call.params["ms"].as_u64().unwrap_or(0))).await;
                Ok(call.params["value"].clone())
            }
            "exec" => call.host().exec("/t", "hello", json!({"x": 1})).await,
            "log" => {
                crate::info("from the handler").await;
                Ok(Value::Null)
            }
            "panic" => panic!("boom"),
            "fail" => Err("nope".into()),
            op => Err(format!("unknown op {op}")),
        }
    }

    #[tokio::test]
    #[serial]
    async fn concurrent_calls_answer_in_finishing_order() {
        let (mut host, task) = start(Concurrency::Concurrent, handler);
        host.request(1, "sleep", json!({"ms": 300, "value": "slow"})).await;
        host.request(2, "sleep", json!({"ms": 0, "value": "fast"})).await;
        host.request(3, "fail", json!({})).await;

        let mut answers = [host.next().await, host.next().await];
        assert_eq!(host.next().await, json!({"kind": "response", "id": 1, "ok": true, "result": "slow"}));
        answers.sort_by_key(|a| a["id"].as_u64());
        assert_eq!(answers[0], json!({"kind": "response", "id": 2, "ok": true, "result": "fast"}));
        assert_eq!(answers[1], json!({"kind": "response", "id": 3, "ok": false, "error": "nope"}));

        drop(host);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn one_at_a_time_keeps_arrival_order() {
        let (mut host, task) = start(Concurrency::OneAtATime, handler);
        host.request(1, "sleep", json!({"ms": 200, "value": 1})).await;
        host.request(2, "sleep", json!({"ms": 0, "value": 2})).await;
        assert_eq!(host.next().await["id"], 1);
        assert_eq!(host.next().await["id"], 2);
        drop(host);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn a_handler_can_call_the_host_while_it_serves() {
        let (mut host, task) = start(Concurrency::OneAtATime, handler);
        host.request(1, "exec", json!({})).await;

        let asked = host.next().await;
        assert_eq!(asked["kind"], "request");
        assert_eq!(asked["op"], OP_EXEC);
        assert_eq!(asked["invocation_id"], "inv-1");
        assert_eq!(asked["params"], json!({"path": "/t", "name": "hello", "params": {"x": 1}}));
        host.send(json!({
            "kind": "response", "id": asked["id"], "ok": true,
            "result": {"action": "/t/hello", "result": 42, "success": true}
        }))
        .await;

        assert_eq!(host.next().await, json!({"kind": "response", "id": 1, "ok": true, "result": 42}));
        drop(host);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn a_refused_host_call_is_the_handlers_error() {
        let (mut host, task) = start(Concurrency::Concurrent, handler);
        host.request(1, "exec", json!({})).await;
        let asked = host.next().await;
        host.send(json!({"kind": "response", "id": asked["id"], "ok": false, "error": "out of scope"}))
            .await;
        assert_eq!(
            host.next().await,
            json!({"kind": "response", "id": 1, "ok": false, "error": "out of scope"})
        );
        drop(host);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn shutdown_lets_running_calls_finish_and_refuses_new_ones() {
        let (mut host, task) = start(Concurrency::Concurrent, handler);
        host.request(1, "sleep", json!({"ms": 300, "value": "done"})).await;
        host.send(json!({"kind": "request", "id": 2, "op": "shutdown"})).await;
        host.request(3, "sleep", json!({"ms": 0, "value": "late"})).await;

        assert_eq!(host.next().await, json!({"kind": "response", "id": 2, "ok": true, "result": null}));
        let refused = host.next().await;
        assert_eq!(refused["id"], 3);
        assert_eq!(refused["ok"], false);
        assert_eq!(host.next().await["result"], "done");
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("serve should return once the running call finished")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn a_closed_stdin_fails_waiting_host_calls_and_returns() {
        let (mut host, task) = start(Concurrency::Concurrent, handler);
        host.request(1, "exec", json!({})).await;
        assert_eq!(host.next().await["op"], OP_EXEC);
        let FakeHost { to_bg, mut from_bg } = host;
        drop(to_bg);

        let line = from_bg.next_line().await.unwrap().unwrap();
        let answer: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(answer["id"], 1);
        assert!(answer["error"].as_str().unwrap().contains("stopped answering"), "{answer}");
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn a_panicking_handler_still_gets_an_answer() {
        let (mut host, task) = start(Concurrency::OneAtATime, handler);
        host.request(1, "panic", json!({})).await;
        let answer = host.next().await;
        assert_eq!(answer["ok"], false);
        assert!(answer["error"].as_str().unwrap().contains("panicked"), "{answer}");
        // Still serving.
        host.request(2, "sleep", json!({"value": "ok"})).await;
        assert_eq!(host.next().await["result"], "ok");
        drop(host);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn logging_goes_to_the_console_of_the_call_being_served() {
        std::env::remove_var("SOL_LOG_DIR");
        let (mut host, task) = start(Concurrency::Concurrent, handler);
        host.request(1, "log", json!({})).await;

        let print = host.next().await;
        assert_eq!(print["kind"], "request");
        assert_eq!(print["op"], OP_CONSOLE_PRINT);
        assert_eq!(print["invocation_id"], "inv-1");
        assert_eq!(
            print["params"],
            json!({"level": "info", "message": "from the handler", "invocation_id": "inv-1"})
        );
        assert_eq!(host.next().await["id"], 1);

        drop(host);
        task.await.unwrap().unwrap();
        assert!(!log_to_host("info", "after serve returned", None));
    }
}

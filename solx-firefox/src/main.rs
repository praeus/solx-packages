//! solx-firefox — manages one persistent, dedicated-profile Firefox instance
//! with Marionette enabled, so solx-mcp's `firefox` server entry can use
//! `--connect-existing` and share a single browser session across tool
//! calls instead of solx-mcp's normal per-invocation spawn-and-teardown model
//! (which is right for stateless MCP servers, but wrong for stateful
//! browser automation).
//!
//! Ships as two solx `Command` actions (`firefox-start`, `firefox-stop`)
//! plus two `Script` actions (`firefox-mcp-setup`, `firefox-mcp-teardown`)
//! that chain start/stop with solx-mcp's import/remove in one call.
//!
//! ## Key difference from sol-firefox (sol ecosystem)
//!
//! solx-core's `run_command` passes parameters as JSON on **stdin**, not via
//! the `SOL_PARAMS` env var. There is no `command_actions` registry in solx
//! — `fn_name` is the literal shell command, and `action_config.cwd` is set
//! on the action itself.

mod process;

use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "solx-firefox")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Override the solx-firefox package home directory. Defaults to the
    /// parent of the directory containing this binary (since the binary
    /// lives in `bin/`, the package root is one level up).
    #[arg(long, global = true)]
    home: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch (or reuse) the dedicated-profile Firefox with Marionette enabled.
    Start {
        #[arg(long)]
        headless: bool,
        #[arg(long)]
        firefox_path: Option<String>,
        #[arg(long)]
        marionette_port: Option<u16>,
        #[arg(long)]
        start_timeout_secs: Option<u64>,
    },
    /// Stop the managed Firefox instance.
    Stop,
    /// Report current running state without side effects.
    Status,
}

fn print_json(value: &Value) {
    println!("{}", serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()));
}

/// Read the caller-supplied message/arguments from **stdin**.
///
/// solx-core's `run_command` (in `solx-actions/src/exec.rs`) writes the
/// params JSON to the child's stdin — no `SOL_PARAMS` env var exists in
/// solx. This is the primary difference from sol-firefox.
fn stdin_params() -> Value {
    use std::io::Read;
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return json!({});
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return json!({});
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| json!({}))
}

const DEFAULT_PORT: u16 = 2828;
const DEFAULT_START_TIMEOUT_SECS: u64 = 20;

fn main() {
    // Plain `fn main` (no tokio runtime) — this binary is entirely
    // synchronous, so `solx_package_log::blocking` is used everywhere below
    // rather than the crate's async entry points. See that module's docs
    // for why: calling the async ones directly with no runtime panics.
    solx_package_log::init("solx-firefox");

    let cli = Cli::parse();
    let home = process::resolve_home(cli.home.as_deref());

    let result = match cli.command {
        Commands::Start { headless, firefox_path, marionette_port, start_timeout_secs } => {
            run_start(&home, headless, firefox_path, marionette_port, start_timeout_secs)
        }
        Commands::Stop => run_stop(&home),
        Commands::Status => run_status(&home),
    };

    match result {
        Ok(value) => {
            let is_error = matches!(value.get("status").and_then(Value::as_str), Some("start_timeout"));
            print_json(&value);
            if is_error {
                std::process::exit(1);
            }
        }
        Err(e) => {
            solx_package_log::blocking::error(&format!("fatal: {e}"));
            print_json(&json!({ "error": e.to_string() }));
            std::process::exit(1);
        }
    }
}

fn run_start(
    home: &std::path::Path,
    headless_flag: bool,
    firefox_path_arg: Option<String>,
    port_arg: Option<u16>,
    start_timeout_arg: Option<u64>,
) -> anyhow::Result<Value> {
    let params = stdin_params();
    let headless = headless_flag || params.get("headless").and_then(Value::as_bool).unwrap_or(false);
    let port = port_arg
        .or_else(|| params.get("marionette_port").and_then(Value::as_u64).map(|p| p as u16))
        .unwrap_or(DEFAULT_PORT);
    let start_timeout_secs = start_timeout_arg
        .or_else(|| params.get("start_timeout_secs").and_then(Value::as_u64))
        .unwrap_or(DEFAULT_START_TIMEOUT_SECS);

    // Check the port FIRST, before touching state.json: a second Firefox
    // can't bind the same Marionette port anyway, so if something is
    // already listening, we must not spawn another instance.
    if process::port_is_open(port, Duration::from_millis(300)) {
        if let Some(state) = process::load_state(home) {
            if state.port == port && process::pid_is_alive(state.pid) {
                solx_package_log::blocking::info(&format!(
                    "firefox already running (pid={}, port={port})",
                    state.pid
                ));
                return Ok(json!({ "status": "already_running", "pid": state.pid, "port": port }));
            }
        }
        // Something is listening on this port but we can't attribute it to
        // our own state — never assume ownership of a process we didn't
        // launch and can't identify.
        solx_package_log::blocking::warn(&format!(
            "port {port} is already in use by a process we don't own; not starting"
        ));
        return Ok(json!({ "status": "already_running_external", "port": port }));
    }

    let firefox_path = process::resolve_firefox_path(firefox_path_arg.as_deref())?;
    let profile_dir = process::profile_dir(home);
    std::fs::create_dir_all(&profile_dir)?;

    // firefox-devtools-mcp's `--connect-existing` needs a WebDriver BiDi
    // `webSocketUrl` in the Marionette session, which Firefox only
    // advertises when also launched with `--remote-debugging-port` (see
    // "Connected to Firefox via Marionette, but the session has no
    // WebDriver BiDi endpoint" if this flag is missing). The port number
    // itself isn't passed to the MCP server separately — it discovers the
    // actual websocket URL through the Marionette session response.
    let remote_debug_port = port + 100;
    let mut args = vec![
        "-marionette".to_string(),
        "-marionette-port".to_string(),
        port.to_string(),
        "-remote-debugging-port".to_string(),
        remote_debug_port.to_string(),
        "-profile".to_string(),
        profile_dir.to_string_lossy().to_string(),
        "-no-remote".to_string(),
    ];
    if headless {
        args.push("-headless".to_string());
    }

    solx_package_log::blocking::info(&format!(
        "spawning firefox at {} (port={port}, headless={headless})",
        firefox_path.display()
    ));
    let child = process::spawn_detached(&firefox_path, &args)
        .map_err(|e| anyhow::anyhow!("failed to spawn '{}': {e}", firefox_path.display()))?;
    let pid = child.id();

    // Write state.json immediately after spawn() returns — before the
    // readiness poll — so a later `stop` can always find the right PID even
    // if this `start` call itself gets killed (e.g. by a Script action step
    // timeout) mid-poll.
    process::write_state(
        home,
        &process::State {
            pid,
            port,
            started_at: process::unix_timestamp(),
            status: "starting".to_string(),
        },
    )?;

    if process::wait_for_port(port, Duration::from_secs(start_timeout_secs)) {
        process::write_state(
            home,
            &process::State {
                pid,
                port,
                started_at: process::unix_timestamp(),
                status: "running".to_string(),
            },
        )?;
        solx_package_log::blocking::info(&format!("firefox started (pid={pid}, port={port})"));
        Ok(json!({ "status": "started", "pid": pid, "port": port }))
    } else {
        solx_package_log::blocking::warn(&format!(
            "firefox did not open the marionette port within {start_timeout_secs}s (pid={pid}, port={port})"
        ));
        Ok(json!({ "status": "start_timeout", "pid": pid, "port": port }))
    }
}

fn run_stop(home: &std::path::Path) -> anyhow::Result<Value> {
    let Some(state) = process::load_state(home) else {
        return Ok(json!({ "status": "not_running" }));
    };

    if !process::pid_is_alive(state.pid) {
        process::remove_state(home);
        return Ok(json!({ "status": "not_running" }));
    }

    process::kill_tree(state.pid)?;
    process::remove_state(home);
    solx_package_log::blocking::info(&format!("firefox stopped (pid={})", state.pid));
    Ok(json!({ "status": "stopped", "pid": state.pid }))
}

fn run_status(home: &std::path::Path) -> anyhow::Result<Value> {
    let Some(state) = process::load_state(home) else {
        return Ok(json!({ "status": "not_running" }));
    };
    let alive = process::pid_is_alive(state.pid);
    let port_open = process::port_is_open(state.port, Duration::from_millis(300));
    if !alive {
        return Ok(json!({ "status": "not_running", "stale_pid": state.pid }));
    }
    Ok(json!({
        "status": if port_open { "running" } else { "starting" },
        "pid": state.pid,
        "port": state.port,
        "started_at": state.started_at,
    }))
}

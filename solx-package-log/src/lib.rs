//! Shared logging for solx-core Command-action packages.
//!
//! One call site (`info`/`warn`/`error`/`with_data`) does three things:
//!
//! 1. Always mirrors to stderr.
//! 2. Appends to `$SOL_LOG_DIR/<package>.log` if `SOL_LOG_DIR` is set —
//!    `run_command` sets this on every spawned Command action
//!    (solx-core's `docs/console-implementation-plan.md`, §9/Phase 0), so
//!    this works with zero configuration when run as a solx action.
//! 3. Best-effort POSTs to the solx-core console loopback if
//!    `SOLX_CONSOLE_URL`/`SOLX_CONSOLE_TOKEN` are set — `run_command` sets
//!    these too (same doc, §8a). A dead/slow loopback never blocks or fails
//!    the caller: the POST has a short timeout and every error is
//!    swallowed.
//!
//! Any of the three can be independently absent — a binary run by hand
//! outside solx-core (no env vars set at all) still gets its stderr output,
//! exactly as if this crate weren't here.
//!
//! Call [`init`] once at startup with the package's own name, before any
//! logging call — it's what names the log file. Skipping it just means a
//! generic file name.
//!
//! ## Sync callers
//!
//! `info`/`warn`/`error`/`with_data` are `async fn`s, for packages that
//! already run under `#[tokio::main]` (every solx-core Command package
//! except `solx-firefox`, as of this writing). A caller with no ambient
//! tokio runtime should use the [`blocking`] module instead — calling the
//! async functions directly with no runtime panics.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

static PACKAGE_NAME: OnceLock<String> = OnceLock::new();

/// Name this process's log lines and log file after `package_name`. Call
/// once, before any logging call — e.g.
/// `solx_package_log::init("solx-media")` at the top of `main()`. Optional:
/// without it, the log file falls back to `solx-package.log`.
pub fn init(package_name: &str) {
    let _ = PACKAGE_NAME.set(package_name.to_string());
}

fn package_name() -> &'static str {
    PACKAGE_NAME.get().map(String::as_str).unwrap_or("solx-package")
}

pub async fn info(message: &str) {
    log("info", message, None).await;
}

pub async fn warn(message: &str) {
    log("warn", message, None).await;
}

pub async fn error(message: &str) {
    log("error", message, None).await;
}

/// Like the level-specific functions above, with a structured payload
/// alongside the message — e.g. `{"page": 3, "total": 12}` for progress.
/// `level` is free-form; the console schema treats
/// `debug`/`info`/`warn`/`error`/`chunk` as the conventional values.
pub async fn with_data(level: &str, message: &str, data: Value) {
    log(level, message, Some(data)).await;
}

async fn log(level: &str, message: &str, data: Option<Value>) {
    let ts = chrono::Utc::now().to_rfc3339();
    eprintln!("[{ts}] {level} {message}");

    write_to_file(&ts, level, message);
    post_to_console(level, message, data).await;
}

fn write_to_file(ts: &str, level: &str, message: &str) {
    let Some(dir) = std::env::var_os("SOL_LOG_DIR").map(PathBuf::from) else {
        return;
    };
    if dir.as_os_str().is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.log", package_name()));
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(file, "[{ts}] {level} {message}");
}

/// Best-effort: a dead/slow loopback (or no `SOLX_CONSOLE_URL`/`TOKEN` at
/// all — e.g. the binary run by hand outside solx-core) must never block or
/// fail the caller.
async fn post_to_console(level: &str, message: &str, data: Option<Value>) {
    let (Ok(url), Ok(token)) = (
        std::env::var("SOLX_CONSOLE_URL"),
        std::env::var("SOLX_CONSOLE_TOKEN"),
    ) else {
        return;
    };
    if url.is_empty() || token.is_empty() {
        return;
    }
    let body = serde_json::json!({ "level": level, "message": message, "data": data });
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(2)).build() else {
        return;
    };
    let _ = client.post(url).bearer_auth(token).json(&body).send().await;
}

/// Sync entry points for a caller with no ambient tokio runtime (as of this
/// writing, only `solx-firefox` among solx-core's Command packages). Each
/// call runs on a runtime built once and reused — safe here specifically
/// because these callers never nest it inside another one; calling this
/// from inside an existing tokio runtime panics (`block_on` inside
/// `block_on`), same as `reqwest::blocking` would.
pub mod blocking {
    use serde_json::Value;
    use std::sync::OnceLock;
    use tokio::runtime::Runtime;

    fn runtime() -> &'static Runtime {
        static RT: OnceLock<Runtime> = OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to start solx-package-log's blocking runtime")
        })
    }

    pub fn info(message: &str) {
        runtime().block_on(super::info(message));
    }

    pub fn warn(message: &str) {
        runtime().block_on(super::warn(message));
    }

    pub fn error(message: &str) {
        runtime().block_on(super::error(message));
    }

    pub fn with_data(level: &str, message: &str, data: Value) {
        runtime().block_on(super::with_data(level, message, data));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::io::Read;

    /// Env vars are process-global, so every test that touches them runs
    /// `#[serial]` — parallel tests would otherwise race on the same vars.
    fn clear_env() {
        std::env::remove_var("SOL_LOG_DIR");
        std::env::remove_var("SOLX_CONSOLE_URL");
        std::env::remove_var("SOLX_CONSOLE_TOKEN");
    }

    #[tokio::test]
    #[serial]
    async fn with_no_env_vars_set_nothing_panics_and_nothing_is_written() {
        clear_env();
        info("hello").await;
        // No file to check — SOL_LOG_DIR was never set, so there's no
        // directory to look in. Reaching this line without panicking or
        // hanging (the console POST must not block on a missing target) is
        // the assertion.
    }

    #[tokio::test]
    #[serial]
    async fn writes_to_sol_log_dir_using_the_initialized_package_name() {
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SOL_LOG_DIR", dir.path());
        init("solx-test-package");

        info("first line").await;
        warn("second line").await;

        let path = dir.path().join("solx-test-package.log");
        let mut contents = String::new();
        std::fs::File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
        assert!(contents.contains("info first line"), "{contents}");
        assert!(contents.contains("warn second line"), "{contents}");
        clear_env();
    }

    #[tokio::test]
    #[serial]
    async fn missing_sol_log_dir_is_a_silent_no_op_not_a_panic() {
        clear_env();
        std::env::set_var("SOL_LOG_DIR", "");
        info("no crash please").await;
        clear_env();
    }

    /// Minimal capture server: records the request path, headers, and body
    /// of the first POST it receives.
    async fn start_capture_server() -> (String, tokio::sync::oneshot::Receiver<(String, String)>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let auth = req
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                .unwrap_or("")
                .to_string();
            let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let _ = tx.send((auth, body));
            let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes()).await;
        });
        (format!("http://{addr}/print"), rx)
    }

    #[tokio::test]
    #[serial]
    async fn posts_to_the_console_with_the_bearer_token_and_json_body() {
        clear_env();
        let (url, rx) = start_capture_server().await;
        std::env::set_var("SOLX_CONSOLE_URL", &url);
        std::env::set_var("SOLX_CONSOLE_TOKEN", "tok-123");

        with_data("warn", "disk almost full", serde_json::json!({"pct": 91})).await;

        let (auth, body) = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("server should have received a request")
            .unwrap();
        assert!(auth.to_ascii_lowercase().contains("bearer tok-123"), "{auth}");
        let parsed: Value = serde_json::from_str(&body).expect("body should be JSON");
        assert_eq!(parsed["level"], "warn");
        assert_eq!(parsed["message"], "disk almost full");
        assert_eq!(parsed["data"]["pct"], 91);
        clear_env();
    }

    #[tokio::test]
    #[serial]
    async fn a_dead_console_url_does_not_hang_or_panic() {
        clear_env();
        // Nothing listens here — a fast, deterministic connection refusal.
        std::env::set_var("SOLX_CONSOLE_URL", "http://127.0.0.1:1/print");
        std::env::set_var("SOLX_CONSOLE_TOKEN", "whatever");

        tokio::time::timeout(Duration::from_secs(5), info("should not hang"))
            .await
            .expect("a dead console target must not block the caller");
        clear_env();
    }

    #[test]
    fn blocking_module_works_with_no_ambient_runtime() {
        // This test function is deliberately not `#[tokio::test]` — the
        // whole point of `blocking` is safety with zero ambient runtime,
        // mirroring solx-firefox's plain `fn main()`.
        clear_env();
        blocking::info("from a sync caller");
    }
}

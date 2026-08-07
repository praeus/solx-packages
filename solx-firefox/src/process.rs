//! State-file management, Firefox binary resolution, and cross-platform
//! detached-process spawn/kill helpers.
//!
//! Detachment uses only stable `std` APIs — no extra process-management
//! crate is needed:
//! - `Stdio::null()` on all three streams is the *load-bearing* fix: solx's
//!   `Command` action dispatch waits on `cmd.output()`, which reads
//!   stdout/stderr to EOF. An inherited pipe handle held open by the Firefox
//!   grandchild would keep that read pending until Firefox itself exits,
//!   silently defeating the whole point of detaching.
//! - Unix: `CommandExt::process_group(0)` (stabilized since Rust 1.64) makes
//!   the child its own process-group leader, a kernel-level property that
//!   persists after our own process exits — so a later, separate `stop`
//!   invocation can signal the whole group (including Firefox's content/GPU
//!   child processes) via the negated PID.
//! - Windows: `CommandExt::creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)`.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Resolve the solx-firefox package home directory:
/// 1. Explicit CLI `--home` flag
/// 2. `SOLX_FIREFOX_HOME` env var
/// 3. Parent of the current executable (since the binary lives in `bin/`,
///    the package root is one level up)
pub fn resolve_home(explicit: Option<&str>) -> PathBuf {
    if let Some(dir) = explicit {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("SOLX_FIREFOX_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    // Binary is at <package>/bin/solx-firefox[.exe]; package root is ../.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn profile_dir(home: &Path) -> PathBuf {
    home.join("profile")
}

pub fn state_path(home: &Path) -> PathBuf {
    home.join("state.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub pid: u32,
    pub port: u16,
    pub started_at: String,
    pub status: String,
}

pub fn load_state(home: &Path) -> Option<State> {
    let raw = std::fs::read_to_string(state_path(home)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write `state.json` atomically (write to a `.tmp` sibling, then rename).
pub fn write_state(home: &Path, state: &State) -> anyhow::Result<()> {
    std::fs::create_dir_all(home)?;
    let final_path = state_path(home);
    let tmp_path = home.join("state.json.tmp");
    let body = serde_json::to_string_pretty(state)?;
    std::fs::write(&tmp_path, body)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

pub fn remove_state(home: &Path) {
    let _ = std::fs::remove_file(state_path(home));
}

pub fn unix_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Quick, non-blocking-ish probe: is something accepting TCP connections on
/// this port right now?
pub fn port_is_open(port: u16, timeout: Duration) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// Poll the port until it accepts connections or `overall_timeout` elapses.
pub fn wait_for_port(port: u16, overall_timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + overall_timeout;
    loop {
        if port_is_open(port, Duration::from_millis(300)) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Is a process with this PID currently alive? Shells out to a small
/// platform utility rather than adding a process-enumeration dependency for
/// this single yes/no check.
pub fn pid_is_alive(pid: u32) -> bool {
    if cfg!(windows) {
        let output = Command::new("tasklist.exe")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match output {
            Ok(out) => String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
            Err(_) => false,
        }
    } else {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// Kill the process (and, on both platforms, its process tree/group).
pub fn kill_tree(pid: u32) -> anyhow::Result<()> {
    if cfg!(windows) {
        let status = Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/F", "/T"])
            .status()?;
        if !status.success() {
            anyhow::bail!("taskkill exited with {:?}", status.code());
        }
    } else {
        // Firefox was spawned as its own process-group leader (pgid == pid),
        // so signaling the negated PID reaches its whole group, including
        // content/GPU child processes.
        let status = Command::new("kill")
            .args(["--", &format!("-{pid}")])
            .status()?;
        if !status.success() {
            anyhow::bail!("kill exited with {:?}", status.code());
        }
    }
    Ok(())
}

/// Resolve the Firefox binary: explicit override, then common per-OS install
/// locations, then PATH lookup.
pub fn resolve_firefox_path(explicit: Option<&str>) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!("--firefox-path '{path}' does not exist");
    }

    let candidates: &[&str] = if cfg!(windows) {
        &[
            "C:/Program Files/Mozilla Firefox/firefox.exe",
            "C:/Program Files (x86)/Mozilla Firefox/firefox.exe",
        ]
    } else if cfg!(target_os = "macos") {
        &["/Applications/Firefox.app/Contents/MacOS/firefox"]
    } else {
        &["/usr/bin/firefox", "/usr/local/bin/firefox", "/snap/bin/firefox"]
    };

    for candidate in candidates {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Ok(p);
        }
    }

    let program = if cfg!(windows) { "firefox.exe" } else { "firefox" };
    Ok(PathBuf::from(program))
}

/// Spawn Firefox as a fully detached background process. See module docs
/// for why each piece here matters.
pub fn spawn_detached(program: &Path, args: &[String]) -> io::Result<Child> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 0 => new process group whose pgid equals the child's own pid.
        cmd.process_group(0);
    }

    cmd.spawn()
}

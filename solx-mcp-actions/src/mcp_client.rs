//! Thin wrapper around `rmcp`'s stdio client: connect to a configured MCP
//! server (spawned as a child process), list its tools, and call a tool.
//!
//! One-shot use (`invoke`) opens a session, calls one tool and closes it.
//! The background command (`serve`) keeps one session for its whole life and
//! has to be able to throw it away and open a new one when the MCP server
//! dies — see [`crate::serve`]. The session is therefore held behind an
//! [`Arc`], so a call can be made through a shared reference while another
//! task replaces it.

use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RunningService, ServiceError, ServiceExt};
use rmcp::transport::TokioChildProcess;
use rmcp::RoleClient;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tokio::process::Command;

use crate::config::ServerDef;

/// Windows' `CREATE_NO_WINDOW`: run a console program without giving it a
/// console window. Ignored if `CREATE_NEW_CONSOLE` or `DETACHED_PROCESS` is
/// also set, neither of which is.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Quote an argument for the target shell only if it needs it — simple
/// tokens (flags, package names, plain paths) pass through unmodified so
/// the common case reads naturally in `mcp-servers.json`.
fn shell_quote(s: &str) -> String {
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '&' | '|' | '<' | '>' | '^')) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

fn build_command_line(command: &str, args: &[String]) -> String {
    let mut parts = vec![shell_quote(command)];
    parts.extend(args.iter().map(|a| shell_quote(a)));
    parts.join(" ")
}

/// The file a `command` names, if it names one: a command containing a path
/// separator, taken relative to `base` (the directory holding
/// `mcp-servers.json`) unless absolute, and tried with the platform's
/// executable suffix if it doesn't exist as written. So
/// `../solx-firefox/bin/mcp-server` finds the sibling package's binary on
/// every platform. A bare name (`npx`) is left to the shell's PATH lookup.
fn resolve_program(command: &str, base: &Path) -> Option<PathBuf> {
    if !command.contains(['/', '\\']) {
        return None;
    }
    let path = base.join(command);
    let with_suffix = PathBuf::from(format!("{}{}", path.display(), std::env::consts::EXE_SUFFIX));
    [path, with_suffix].into_iter().find(|p| p.is_file())
}

/// Whether `path` needs a shell to run: Windows batch files (`.cmd`/`.bat`)
/// can't be exec'd directly.
fn needs_shell(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

pub struct McpSession {
    service: Arc<RunningService<RoleClient, ()>>,
}

impl McpSession {
    /// Spawn the server's stdio process and complete the MCP handshake.
    /// `base` is the directory holding `mcp-servers.json`; a relative path
    /// in `command` resolves against it (see [`resolve_program`]).
    ///
    /// A `command` that resolves to an executable file is spawned directly,
    /// with no shell in between, so its path and args reach it verbatim
    /// (packages installed from a zip live under the user's profile, whose
    /// path can contain spaces). Anything else — a bare name like `npx`, or
    /// a `.cmd`/`.bat` — has `command`/`args` joined into one string and run
    /// through the platform shell (`cmd.exe /C` on Windows, `sh -c`
    /// elsewhere), the same convention solx-core's own `Command` action-type
    /// dispatch uses: `npx` on Windows is a `.cmd` shim that `CreateProcess`
    /// cannot exec directly.
    pub async fn connect(def: &ServerDef, base: &Path) -> anyhow::Result<Self> {
        let program = resolve_program(&def.command, base);
        let mut cmd = match program.as_deref() {
            Some(path) if !needs_shell(path) => {
                let mut c = Command::new(path);
                c.args(&def.args);
                c
            }
            _ => {
                let command = program.map_or_else(|| def.command.clone(), |p| p.display().to_string());
                let line = build_command_line(&command, &def.args);
                let (shell, flag) = if cfg!(windows) { ("cmd.exe", "/C") } else { ("sh", "-c") };
                let mut c = Command::new(shell);
                c.arg(flag).arg(&line);
                c
            }
        };
        for (k, v) in &def.env {
            cmd.env(k, v);
        }
        if let Some(cwd) = &def.cwd {
            cmd.current_dir(cwd);
        }

        // On Windows, a console program spawned without a flag opens a
        // console window of its own. The MCP server is a console app and is
        // spawned once per tool call, so without this it reads as a cmd
        // window flashing on and off with every call. CREATE_NO_WINDOW runs
        // it without one; the stdio pipes the protocol rides on are
        // unaffected, and the child keeps its own stdio handles.
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| anyhow::anyhow!("failed to spawn MCP server '{}': {e}", def.command))?;
        // `()` is a no-op `ClientHandler` — we only need `tools/list` and
        // `tools/call`, no sampling/roots/elicitation callbacks.
        let service = ()
            .serve(transport)
            .await
            .map_err(|e| anyhow::anyhow!("MCP initialize handshake failed: {e}"))?;
        Ok(Self {
            service: Arc::new(service),
        })
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<Tool>> {
        self.service
            .list_all_tools()
            .await
            .map_err(|e| anyhow::anyhow!("tools/list failed: {e}"))
    }

    /// Call a tool. The raw [`ServiceError`] is returned so the caller can
    /// tell a dead MCP server (which needs a new session) from a tool that
    /// merely failed — see [`Self::is_transport_failure`].
    pub async fn call_tool_raw(
        &self,
        tool: &str,
        arguments: Value,
    ) -> Result<CallToolResult, ServiceError> {
        let arguments = match arguments {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("value".to_string(), other);
                Some(map)
            }
        };
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        self.service.call_tool(params).await
    }

    /// Call a tool, with the error turned into a message.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> anyhow::Result<CallToolResult> {
        self.call_tool_raw(tool, arguments)
            .await
            .map_err(|e| anyhow::anyhow!("tools/call '{tool}' failed: {e}"))
    }

    /// Whether the failure means the MCP server is *gone* — the transport
    /// closed or a send failed — rather than the tool itself failing. Only
    /// these are worth replacing the session for.
    pub fn is_transport_failure(err: &ServiceError) -> bool {
        matches!(
            err,
            ServiceError::TransportClosed | ServiceError::TransportSend(_)
        )
    }

    /// Shut the session down, which cancels the service and kills the MCP
    /// server process (rmcp kills the child when the transport is dropped).
    ///
    /// Best effort: `cancel` consumes the service, and when something else
    /// still holds a reference to it the session is dropped instead — rmcp's
    /// own drop guard cancels it then, just not awaited.
    pub async fn close(self) -> anyhow::Result<()> {
        match Arc::try_unwrap(self.service) {
            Ok(service) => {
                service
                    .cancel()
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to shut down MCP session: {e}"))?;
                Ok(())
            }
            Err(service) => {
                drop(service);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod program_tests {
    use super::*;

    /// `<tmp>/solx-mcp-resolve-<name>/{home,sibling/bin}`, with `sibling/bin/server<EXE_SUFFIX>`.
    fn layout(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("solx-mcp-resolve-{name}-{}", std::process::id()));
        let home = root.join("home");
        let bin = root.join("sibling").join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(format!("server{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&exe, b"").unwrap();
        (home, exe)
    }

    #[test]
    fn relative_command_resolves_against_base_with_exe_suffix() {
        let (home, _) = layout("relative");
        let expected = home.join(format!("../sibling/bin/server{}", std::env::consts::EXE_SUFFIX));
        assert_eq!(resolve_program("../sibling/bin/server", &home), Some(expected));
        assert_eq!(resolve_program("../sibling/bin/missing", &home), None);
    }

    #[test]
    fn absolute_command_is_kept() {
        let (home, exe) = layout("absolute");
        assert_eq!(resolve_program(exe.to_str().unwrap(), &home), Some(exe));
    }

    #[test]
    fn bare_name_is_left_to_the_shell() {
        let (home, _) = layout("bare");
        assert_eq!(resolve_program("npx", &home), None);
    }

    #[test]
    fn batch_files_need_a_shell() {
        assert!(needs_shell(Path::new("C:/x/npx.cmd")));
        assert!(needs_shell(Path::new("C:/x/run.BAT")));
        assert!(!needs_shell(Path::new("C:/x/mcp-server.exe")));
        assert!(!needs_shell(Path::new("/x/mcp-server")));
    }
}

//! Thin wrapper around `rmcp`'s stdio client: connect to a configured MCP
//! server (spawned as a child process), list its tools, and call a tool.

use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RunningService, ServiceExt};
use rmcp::transport::TokioChildProcess;
use rmcp::RoleClient;
use serde_json::Value;
use tokio::process::Command;

use crate::config::ServerDef;

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

pub struct McpSession {
    service: RunningService<RoleClient, ()>,
}

impl McpSession {
    /// Spawn the server's stdio process and complete the MCP handshake.
    ///
    /// The `command`/`args` from `mcp-servers.json` are joined into one
    /// string and run through the platform shell (`cmd.exe /C` on Windows,
    /// `sh -c` elsewhere) — the same convention solx-core's own `Command`
    /// action-type dispatch uses. This matters in practice because many
    /// real MCP servers are launched via `npx`, which on Windows is a
    /// `.cmd` shim that `CreateProcess` cannot exec directly without going
    /// through a shell.
    pub async fn connect(def: &ServerDef) -> anyhow::Result<Self> {
        let line = build_command_line(&def.command, &def.args);
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd.exe");
            c.arg("/C").arg(&line);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&line);
            c
        };
        for (k, v) in &def.env {
            cmd.env(k, v);
        }
        if let Some(cwd) = &def.cwd {
            cmd.current_dir(cwd);
        }

        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| anyhow::anyhow!("failed to spawn MCP server '{}': {e}", def.command))?;
        // `()` is a no-op `ClientHandler` — we only need `tools/list` and
        // `tools/call`, no sampling/roots/elicitation callbacks.
        let service = ()
            .serve(transport)
            .await
            .map_err(|e| anyhow::anyhow!("MCP initialize handshake failed: {e}"))?;
        Ok(Self { service })
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<Tool>> {
        self.service
            .list_all_tools()
            .await
            .map_err(|e| anyhow::anyhow!("tools/list failed: {e}"))
    }

    pub async fn call_tool(&self, tool: &str, arguments: Value) -> anyhow::Result<CallToolResult> {
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
        self.service
            .call_tool(params)
            .await
            .map_err(|e| anyhow::anyhow!("tools/call '{tool}' failed: {e}"))
    }

    pub async fn close(self) -> anyhow::Result<()> {
        self.service
            .cancel()
            .await
            .map_err(|e| anyhow::anyhow!("failed to shut down MCP session: {e}"))?;
        Ok(())
    }
}

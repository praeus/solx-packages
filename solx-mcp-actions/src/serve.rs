//! The background command: one long-lived process per MCP server, serving
//! every tool imported from that server over solx's background protocol.
//!
//! solx-core starts this process on the first tool call (see
//! `core/docs/background-commands-plan.md`), hands it each tool call as a
//! request, and stops it when the server's tools are removed or the host
//! exits. The MCP server it talks to is spawned once and kept for the life
//! of the process, so a run of tool calls shares one connection — which is
//! the whole point: browser automation and every other stateful MCP server
//! need the state that a per-call process would throw away.
//!
//! ## Which server this process serves
//!
//! The background's name is the MCP server's key in `mcp-servers.json`, and
//! solx-core passes it in `SOLX_BACKGROUND` (`<namespace>/<name>`) when it
//! spawns the process. The namespace is fixed (`solx-mcp-actions`), so the
//! name is the server key.
//!
//! ## Which tool a call means
//!
//! The operation name carries it: `tool:new_page` calls the MCP tool
//! `new_page`. The prefix keeps a tool named `start` or `stop` from being
//! mistaken for one of solx's own reserved operations.
//!
//! ## Stdout belongs to the protocol
//!
//! Every line this process writes to stdout is read by solx-core as a
//! protocol message, and a line that isn't one closes the background. Log
//! with `solx_package_log::{info, warn, error}` — never `println!`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use solx_package_log::background::{self, Call};

use crate::config::{self, ServerDef};
use crate::mcp_client::McpSession;

/// The operation prefix that carries an MCP tool's name: `tool:new_page`.
const TOOL_PREFIX: &str = "tool:";

/// Serve one MCP server's tools until solx-core shuts the background down.
pub async fn serve(home: &Path) -> anyhow::Result<()> {
    let server = server_key()?;
    let servers_cfg = config::load_servers_config(home)?;
    let def = config::find_server(&servers_cfg, &server)?.clone();
    let base = config::servers_config_path(home)
        .parent()
        .unwrap_or(home)
        .to_path_buf();

    let session = Arc::new(Session::new(server, def, base));

    // Connect now, so the first tool call finds the connection warm. This
    // is best effort: a server that isn't up yet (Firefox not started) must
    // not stop the background from existing — the first call connects again
    // and reports the real error.
    {
        let session = session.clone();
        tokio::spawn(async move {
            match session.session().await {
                Ok(_) => {
                    solx_package_log::info(&format!(
                        "mcp server '{}' connected",
                        session.server
                    ))
                    .await;
                }
                Err(e) => {
                    solx_package_log::warn(&format!(
                        "mcp server '{}' is not connected yet: {e}",
                        session.server
                    ))
                    .await;
                }
            }
        });
    }

    let served = background::serve(move |call| {
        let session = session.clone();
        async move { handle(&session, call).await }
    })
    .await;

    match served {
        Ok(()) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("serving the background: {e}")),
    }
}

/// The MCP server this background serves: the `backgroundName` solx-core put
/// in `SOLX_BACKGROUND` (`<namespace>/<name>`).
fn server_key() -> anyhow::Result<String> {
    let raw = std::env::var(background::ENV_BACKGROUND).map_err(|_| {
        anyhow::anyhow!(
            "SOLX_BACKGROUND is not set: 'solx-mcp-actions serve' runs as a solx background \
             command, started by solx-core, not by hand"
        )
    })?;
    parse_server_key(&raw)
}

/// The server name out of a `SOLX_BACKGROUND` value.
fn parse_server_key(raw: &str) -> anyhow::Result<String> {
    let (_, name) = raw.split_once('/').ok_or_else(|| {
        anyhow::anyhow!("SOLX_BACKGROUND is '{raw}', expected '<namespace>/<name>'")
    })?;
    if name.is_empty() {
        return Err(anyhow::anyhow!(
            "SOLX_BACKGROUND is '{raw}': the name is empty"
        ));
    }
    Ok(name.to_string())
}

/// What an operation name asks for.
#[derive(Debug, PartialEq, Eq)]
enum Operation<'a> {
    /// Call the MCP tool of that name.
    Tool(&'a str),
    /// Nothing this background serves.
    Unknown,
}

/// Read the operation name. The tool name rides in the operation, prefixed,
/// so a tool named `start` or `stop` isn't mistaken for one of solx-core's
/// own reserved operations.
fn operation(op: &str) -> Operation<'_> {
    match op.strip_prefix(TOOL_PREFIX) {
        Some(tool) if !tool.is_empty() => Operation::Tool(tool),
        _ => Operation::Unknown,
    }
}

/// One MCP server's live session.
///
/// The session itself is behind an [`Arc`] so calls can run through a shared
/// reference — rmcp's service handles concurrent requests on one session —
/// while the slot holding it is only locked to open or replace it.
struct Session {
    /// The MCP server's key in `mcp-servers.json`, and the name of the
    /// background serving it.
    server: String,
    def: ServerDef,
    /// The directory `mcp-servers.json` lives in; a relative `command` in
    /// the server's definition resolves against it.
    base: PathBuf,
    inner: tokio::sync::Mutex<Option<Arc<McpSession>>>,
}

impl Session {
    fn new(server: String, def: ServerDef, base: PathBuf) -> Self {
        Session {
            server,
            def,
            base,
            inner: tokio::sync::Mutex::new(None),
        }
    }

    /// The live session, opening one if there is none.
    async fn session(&self) -> anyhow::Result<Arc<McpSession>> {
        let mut slot = self.inner.lock().await;
        if let Some(session) = slot.as_ref() {
            return Ok(session.clone());
        }
        // The lock is held across the connect so two calls arriving together
        // spawn one MCP server, not two.
        let session = Arc::new(McpSession::connect(&self.def, &self.base).await?);
        *slot = Some(session.clone());
        Ok(session)
    }

    /// Throw the session away, so the next call opens a fresh MCP server.
    ///
    /// Only the session that is currently stored is dropped: a call that
    /// replaced it in the meantime has already dealt with the failure that
    /// brought us here.
    async fn discard(&self, stale: &Arc<McpSession>) {
        let old = {
            let mut slot = self.inner.lock().await;
            match slot.as_ref() {
                Some(current) if Arc::ptr_eq(current, stale) => slot.take(),
                _ => None,
            }
        };
        if let Some(old) = old {
            // Closing cancels the service and kills the MCP server process.
            // It is awaited outside the lock so a call in flight isn't held
            // up by it.
            match Arc::try_unwrap(old) {
                Ok(session) => {
                    let _ = session.close().await;
                }
                Err(session) => drop(session),
            }
        }
    }
}

/// Handle one request from solx-core.
async fn handle(session: &Arc<Session>, call: Call) -> Result<Value, String> {
    match operation(&call.op) {
        Operation::Tool(tool) => call_tool(session, tool, call.params).await,
        Operation::Unknown => Err(format!("solx-mcp-actions has no operation '{}'", call.op)),
    }
}

/// Call one MCP tool, replacing the session and trying once more if the MCP
/// server turned out to be gone.
async fn call_tool(session: &Arc<Session>, tool: &str, params: Value) -> Result<Value, String> {
    let live = session.session().await.map_err(|e| e.to_string())?;
    match live.call_tool_raw(tool, params.clone()).await {
        Ok(result) => Ok(tool_result(session, tool, result)),
        Err(e) if McpSession::is_transport_failure(&e) => {
            // The MCP server died under us — Firefox was restarted, or the
            // server itself exited. A new process re-attaches to whatever it
            // was attached to, so replace the session and try once more.
            solx_package_log::warn(&format!(
                "mcp server '{}' went away during '{tool}' ({e}); restarting it",
                session.server
            ))
            .await;
            session.discard(&live).await;
            let fresh = session.session().await.map_err(|e| {
                format!(
                    "the MCP server for '{}' is gone and could not be restarted: {e}",
                    session.server
                )
            })?;
            match fresh.call_tool_raw(tool, params).await {
                Ok(result) => Ok(tool_result(session, tool, result)),
                Err(e) => Err(format!("tools/call '{tool}' failed: {e}")),
            }
        }
        Err(e) => Err(format!("tools/call '{tool}' failed: {e}")),
    }
}

/// The result envelope a tool call answers with — the same shape the
/// one-shot `invoke` produced, so the generated result type still describes
/// it.
fn tool_result(session: &Session, tool: &str, result: CallToolResult) -> Value {
    let content = serde_json::to_value(&result.content).unwrap_or(Value::Null);
    if result.is_error.unwrap_or(false) {
        // Most MCP servers report a failure as the error text in the first
        // content entry. Without this we would only know "the tool errored".
        let error_text = result
            .content
            .iter()
            .find_map(|c| {
                serde_json::to_value(c).ok().and_then(|v| {
                    v.get("text")
                        .and_then(|t| t.as_str())
                        .map(str::to_string)
                })
            })
            .unwrap_or_else(|| "(no error text from MCP server)".to_string());
        return json!({
            "error": format!(
                "MCP tool '{}' on server '{}' reported an error: {}",
                tool, session.server, error_text
            ),
            "server": session.server,
            "tool": tool,
            "content": content,
        });
    }
    let mut out = json!({
        "server": session.server,
        "tool": tool,
        "content": content,
    });
    if let Some(structured) = &result.structured_content {
        out["structured_content"] = structured.clone();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_name_comes_out_of_the_background_name() {
        assert_eq!(
            parse_server_key("solx-mcp-actions/firefox").unwrap(),
            "firefox"
        );
        // A namespace with a slash in it would make the split ambiguous, so
        // the name is everything after the first one.
        assert_eq!(parse_server_key("a/b/c").unwrap(), "b/c");
    }

    #[test]
    fn a_malformed_background_name_is_refused() {
        assert!(parse_server_key("firefox").is_err());
        assert!(parse_server_key("solx-mcp-actions/").is_err());
        assert!(parse_server_key("").is_err());
    }

    #[test]
    fn an_operation_names_the_tool_it_calls() {
        assert_eq!(operation("tool:new_page"), Operation::Tool("new_page"));
        // A tool named after one of solx-core's reserved operations is still
        // just a tool.
        assert_eq!(operation("tool:start"), Operation::Tool("start"));
        assert_eq!(operation("tool:stop"), Operation::Tool("stop"));
    }

    #[test]
    fn an_unknown_operation_names_nothing() {
        assert_eq!(operation(""), Operation::Unknown);
        assert_eq!(operation("tool:"), Operation::Unknown);
        assert_eq!(operation("new_page"), Operation::Unknown);
        assert_eq!(operation("start"), Operation::Unknown);
        assert_eq!(operation("stop"), Operation::Unknown);
        assert_eq!(operation("connect"), Operation::Unknown);
    }
}

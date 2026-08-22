//! solx-mcp-actions — MCP (Model Context Protocol) action package for solx-core.
//!
//! Connects to MCP servers over stdio and imports their tools as ordinary
//! solx `Command` actions — no changes to the core `solx-core` repository
//! are required.
//!
//! Ships as three subcommands, all exposed as separate solx actions:
//!
//! - `import`  — connect to a configured MCP server, list its tools via
//!               `tools/list`, and create one solx `Command` action per
//!               tool (e.g. `mcp-filesystem-read-file`).
//! - `invoke`  — the shared command key every generated per-tool action
//!               points at. Recovers which server/tool to call from
//!               `./tool.json` in its own process cwd (set via each
//!               generated action's `action_config.cwd` override), and
//!               reads the call arguments from **stdin** (solx-core's
//!               `run_command` writes params JSON to the child's stdin).
//! - `remove`  — delete a server's previously imported actions/types,
//!               using the manifest `import` wrote as the source of truth.
//!
//! ## Naming
//!
//! This package is named `solx-mcp-actions` (binary `solx-mcp-actions`) to
//! avoid colliding with `solx-mcp`, the MCP **server** in `solx-core` that
//! exposes solx itself as an MCP server. The MCP ecosystem naming
//! convention for imported tool actions (`mcp-<server>-<tool>`, e.g.
//! `mcp-filesystem-read-file`) is unchanged.
//!
//! ## Key difference from sol-mcp (sol ecosystem)
//!
//! solx-core's `run_command` passes parameters as JSON on **stdin**, not via
//! the `SOL_PARAMS` env var. This is the primary change when porting from
//! sol-mcp to solx-mcp-actions. There is no `command_actions` registry in
//! solx — `fn_name` is the literal shell command, and `action_config.cwd`
//! is set on the action itself.

mod config;
mod mcp_client;
mod naming;
mod solx;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use config::{Manifest, ManifestEntry, ServersConfig, ToolDescriptor, ToolFilter};
use mcp_client::McpSession;

#[derive(Parser)]
#[command(name = "solx-mcp-actions")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Override the solx-mcp-actions package home directory. Defaults to the
    /// parent of the directory containing this binary (since the binary
    /// lives in `bin/`, the package root is one level up).
    #[arg(long, global = true)]
    home: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Import an MCP server's tools as solx Command actions.
    Import {
        /// Server key in mcp-servers.json. May also be supplied via
        /// stdin params as {"server": "..."} when invoked as the
        /// solx-mcp-import action.
        server: Option<String>,
        #[arg(long)]
        dry_run: bool,
        /// Stamp this Permission name onto every generated action, scoping
        /// which callers may invoke the imported MCP tools.
        #[arg(long)]
        permission_name: Option<String>,
        /// Only import tools whose raw MCP name matches one of these
        /// patterns (one `*` wildcard allowed per pattern). Overrides, does
        /// not merge with, any `tool_filter.include` in mcp-servers.json.
        #[arg(long)]
        include: Vec<String>,
        /// Never import tools whose raw MCP name matches one of these
        /// patterns, applied after `include`. Overrides, does not merge
        /// with, any `tool_filter.exclude` in mcp-servers.json.
        #[arg(long)]
        exclude: Vec<String>,
    },
    /// Invoke a single MCP tool. Reads {server,tool} from ./tool.json
    /// (process cwd) and call arguments from stdin.
    Invoke,
    /// Remove a previously imported MCP server's actions/types.
    Remove {
        /// Server key. May also be supplied via stdin params as
        /// {"server": "..."}.
        server: Option<String>,
    },
}

fn print_json(value: &Value) {
    println!("{}", serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()));
}

/// Read the caller-supplied message/arguments from **stdin**.
///
/// solx-core's `run_command` (in `solx-actions/src/exec.rs`) writes the
/// params JSON to the child's stdin — no `SOL_PARAMS` env var exists in
/// solx. This is the primary difference from sol-mcp.
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

fn unix_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[tokio::main]
async fn main() {
    solx_package_log::init("solx-mcp-actions");

    let cli = Cli::parse();
    let home = config::resolve_home(cli.home.as_deref());

    let result = match cli.command {
        Commands::Import { server, dry_run, permission_name, include, exclude } => {
            run_import(&home, server, dry_run, permission_name, include, exclude).await
        }
        Commands::Invoke => run_invoke().await,
        Commands::Remove { server } => run_remove(&home, server).await,
    };

    match result {
        Ok(value) => {
            let is_error = value.get("error").is_some();
            print_json(&value);
            if is_error {
                std::process::exit(1);
            }
        }
        Err(e) => {
            solx_package_log::error(&format!("fatal: {e}")).await;
            print_json(&json!({ "error": e.to_string() }));
            std::process::exit(1);
        }
    }
}

fn string_array_param(params: &Value, key: &str) -> Option<Vec<String>> {
    params
        .get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
}

async fn run_import(
    home: &Path,
    server_arg: Option<String>,
    dry_run_flag: bool,
    permission_arg: Option<String>,
    include_arg: Vec<String>,
    exclude_arg: Vec<String>,
) -> anyhow::Result<Value> {
    let params = stdin_params();
    let server = server_arg
        .or_else(|| params.get("server").and_then(|v| v.as_str()).map(str::to_string))
        .ok_or_else(|| anyhow::anyhow!("missing required 'server' (pass as CLI arg or stdin params.server)"))?;
    let dry_run = dry_run_flag || params.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let permission_name = permission_arg
        .or_else(|| params.get("permission_name").and_then(|v| v.as_str()).map(str::to_string));

    // A CLI/stdin-supplied include or exclude entirely replaces (never
    // merges with) the server's default `tool_filter` in mcp-servers.json —
    // see ToolFilter's doc comment for why.
    let include_override = if include_arg.is_empty() { string_array_param(&params, "include") } else { Some(include_arg) };
    let exclude_override = if exclude_arg.is_empty() { string_array_param(&params, "exclude") } else { Some(exclude_arg) };

    solx_package_log::info(&format!("import: server={server} dry_run={dry_run}")).await;

    let servers_cfg = config::load_servers_config(home)?;
    let def = config::find_server(&servers_cfg, &server)?.clone();

    let filter = if include_override.is_some() || exclude_override.is_some() {
        ToolFilter { include: include_override, exclude: exclude_override }
    } else {
        def.tool_filter.clone().unwrap_or_default()
    };

    let session = McpSession::connect(&def).await?;
    let all_tools = session.list_tools().await?;
    let discovered = all_tools.len();
    let tools: Vec<_> = all_tools.into_iter().filter(|t| filter.allows(&t.name)).collect();
    solx_package_log::info(&format!(
        "import: discovered {discovered} tools on '{server}', {} pass the filter", tools.len()
    )).await;

    if dry_run {
        let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        let _ = session.close().await;
        return Ok(json!({ "server": server, "dry_run": true, "tools": names }));
    }

    let servers_config_path = config::servers_config_path(home).to_string_lossy().to_string();
    let mut manifest_entries: Vec<ManifestEntry> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();

    for tool in &tools {
        let tool_name = tool.name.to_string();
        match import_one_tool(home, &server, &tool_name, tool, &servers_config_path, permission_name.as_deref()).await {
            Ok(entry) => manifest_entries.push(entry),
            Err(e) => {
                solx_package_log::warn(&format!("import: tool '{tool_name}' failed: {e}")).await;
                errors.push(json!({ "tool": tool_name, "error": e.to_string() }));
            }
        }
    }

    let _ = session.close().await;

    // If a prior import (with a broader filter, or before this server had a
    // filter at all) created actions for tools that the current filter no
    // longer selects, remove them now — otherwise a narrowing re-import
    // would silently leave those old actions orphaned in solx instead of
    // actually enforcing the new filter.
    let mut tools_pruned: Vec<String> = Vec::new();
    if let Ok(old_manifest) = config::load_manifest(home, &server) {
        let kept: std::collections::HashSet<&str> =
            manifest_entries.iter().map(|e| e.tool.as_str()).collect();
        for stale in old_manifest.tools.iter().filter(|e| !kept.contains(e.tool.as_str())) {
            if let Err(e) = solx::delete_action(&stale.action_path, &stale.action_name).await {
                errors.push(json!({ "entity": stale.action_name, "error": e.to_string() }));
                continue;
            }
            if let Err(e) = solx::delete_type(config::TYPES_PATH, &stale.param_type_name).await {
                errors.push(json!({ "entity": stale.param_type_name, "error": e.to_string() }));
            }
            if let Some(result_type) = &stale.result_type_name {
                if let Err(e) = solx::delete_type(config::TYPES_PATH, result_type).await {
                    errors.push(json!({ "entity": result_type.clone(), "error": e.to_string() }));
                }
            }
            let _ = std::fs::remove_dir_all(&stale.dir);
            tools_pruned.push(stale.action_name.clone());
        }
    }

    let manifest = Manifest {
        server: server.clone(),
        imported_at: unix_timestamp(),
        tools: manifest_entries.clone(),
    };
    config::write_manifest(home, &manifest)?;

    Ok(json!({
        "server": server,
        "tools_imported": manifest_entries.iter().map(|e| e.action_name.clone()).collect::<Vec<_>>(),
        "tools_pruned": tools_pruned,
        "errors": errors,
    }))
}

async fn import_one_tool(
    home: &Path,
    server: &str,
    tool_name: &str,
    tool: &rmcp::model::Tool,
    servers_config_path: &str,
    permission_name: Option<&str>,
) -> anyhow::Result<ManifestEntry> {
    let action = naming::action_name(server, tool_name);
    let action_path = naming::action_path(server);
    let param_type = naming::param_type_name(server, tool_name);
    let param_type_ref = format!("{}/{param_type}", config::TYPES_PATH);
    let tool_dir_sanitized = naming::sanitize(tool_name);
    let dir = config::tool_dir(home, server, &tool_dir_sanitized);

    let descriptor = ToolDescriptor {
        server: server.to_string(),
        tool: tool_name.to_string(),
        servers_config_path: servers_config_path.to_string(),
    };
    config::write_tool_descriptor(&dir, &descriptor)?;

    let schema_value = Value::Object((*tool.input_schema).clone());
    let description = tool
        .description
        .as_ref()
        .map(|d| d.to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("MCP tool '{tool_name}' imported from server '{server}'."));

    solx::new_type(
        config::TYPES_PATH,
        &param_type,
        &json!({
            "description": format!("Input parameters for MCP tool '{tool_name}' on server '{server}'."),
            "schema": schema_value,
        }),
    ).await?;

    // `run_invoke` (below) always wraps whatever the MCP tool returns in the
    // same envelope — {server, tool, content, structured_content?, error?} —
    // regardless of whether the tool itself declares an `output_schema` in
    // its MCP definition (most don't). That envelope shape is something
    // *this* package guarantees, so a result type can always be generated,
    // not just when the upstream tool happens to publish one. When the tool
    // does declare `output_schema`, it's threaded in as the type of
    // `structured_content` instead of left untyped.
    let result_type = naming::result_type_name(server, tool_name);
    let structured_content_schema = match &tool.output_schema {
        Some(schema) => Value::Object((**schema).clone()),
        None => json!({
            "description": "Only present if the MCP tool call returned structuredContent; this tool does not declare a schema for it."
        }),
    };
    solx::new_type(
        config::TYPES_PATH,
        &result_type,
        &json!({
            "description": format!("Result envelope for MCP tool '{tool_name}' on server '{server}', as returned by solx-mcp-actions invoke."),
            "schema": {
                "type": "object",
                "required": ["server", "tool", "content"],
                "properties": {
                    "server": { "type": "string" },
                    "tool": { "type": "string" },
                    "content": {
                        "type": "array",
                        "description": "MCP content blocks returned by the tool (text/image/resource/etc.)."
                    },
                    "structured_content": structured_content_schema,
                    "error": { "type": "string", "description": "Present only when the tool call failed." },
                },
            },
        }),
    ).await?;
    let result_type_ref = format!("{}/{result_type}", config::TYPES_PATH);

    // The generated action is a Command type. `fn_name` is the absolute path
    // to the binary. Using an absolute path avoids `cmd.exe /C` wrapping,
    // which on Windows consumes the parent's stdin pipe instead of
    // forwarding it to the child — making `stdin_params()` see an empty
    // stdin. `action_config.cwd` points to the per-tool directory containing
    // `tool.json`, so `invoke` can read its identity from `./tool.json`.
    let invoke_cmd = if cfg!(windows) {
        format!("{}\\bin\\solx-mcp-actions.exe invoke", home.to_string_lossy())
    } else {
        format!("{}/bin/solx-mcp-actions invoke", home.to_string_lossy())
    };

    let mut action_body = json!({
        "action_type": "command",
        "fn_name": invoke_cmd,
        "caption": format!("MCP: {tool_name}"),
        "category": "mcp",
        "description": description,
        "capabilities": vec!["mcp".to_string(), server.to_string()],
        "phrases": vec![tool_name.to_string(), format!("mcp {tool_name}"), format!("{server} {tool_name}")],
        "param_type_ref": param_type_ref,
        "result_type_ref": result_type_ref,
        "action_config": { "cwd": dir.to_string_lossy() },
    });
    if let Some(perm) = permission_name {
        action_body["permission_name"] = json!(perm);
    }
    solx::new_action(&action_path, &action, &action_body).await?;

    Ok(ManifestEntry {
        tool: tool_name.to_string(),
        action_name: action,
        action_path,
        param_type_name: param_type,
        result_type_name: Some(result_type),
        dir: dir.to_string_lossy().to_string(),
    })
}

async fn run_invoke() -> anyhow::Result<Value> {
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("failed to read current working directory: {e}"))?;
    let descriptor = config::read_tool_descriptor(&cwd)?;
    solx_package_log::info(&format!(
        "invoke: server={} tool={} cwd={}",
        descriptor.server,
        descriptor.tool,
        cwd.display()
    ))
    .await;

    let servers_cfg_path = PathBuf::from(&descriptor.servers_config_path);
    let raw = std::fs::read_to_string(&servers_cfg_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", servers_cfg_path.display()))?;
    let servers_cfg: ServersConfig = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", servers_cfg_path.display()))?;
    let def = config::find_server(&servers_cfg, &descriptor.server)?.clone();

    let params = stdin_params();
    let session = McpSession::connect(&def).await?;
    let call_result = session.call_tool(&descriptor.tool, params).await;
    let _ = session.close().await;
    let call_result = call_result?;

    let content = serde_json::to_value(&call_result.content).unwrap_or(Value::Null);
    let is_error = call_result.is_error.unwrap_or(false);

    if is_error {
        // Extract the actual MCP error message from the content array
        // (most MCP servers return it as a `text` field in the first
        // content entry). Without this we just know "the tool errored"
        // but not *why*.
        let error_text = call_result
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
        solx_package_log::warn(&format!(
            "invoke: tool '{}' on '{}' reported is_error=true: {error_text}",
            descriptor.tool, descriptor.server
        ))
        .await;
        return Ok(json!({
            "error": format!(
                "MCP tool '{}' on server '{}' reported an error: {}",
                descriptor.tool, descriptor.server, error_text
            ),
            "server": descriptor.server,
            "tool": descriptor.tool,
            "content": content,
        }));
    }

    let mut out = json!({
        "server": descriptor.server,
        "tool": descriptor.tool,
        "content": content,
    });
    if let Some(structured) = &call_result.structured_content {
        out["structured_content"] = structured.clone();
    }
    Ok(out)
}

async fn run_remove(home: &Path, server_arg: Option<String>) -> anyhow::Result<Value> {
    let params = stdin_params();
    let server = server_arg
        .or_else(|| params.get("server").and_then(|v| v.as_str()).map(str::to_string))
        .ok_or_else(|| anyhow::anyhow!("missing required 'server' (pass as CLI arg or stdin params.server)"))?;

    solx_package_log::info(&format!("remove: server={server}")).await;

    let manifest = config::load_manifest(home, &server)?;
    let mut removed = Vec::new();
    let mut errors: Vec<Value> = Vec::new();

    for entry in &manifest.tools {
        let mut ok = true;
        if let Err(e) = solx::delete_action(&entry.action_path, &entry.action_name).await {
            ok = false;
            errors.push(json!({ "entity": entry.action_name, "error": e.to_string() }));
        }
        if let Err(e) = solx::delete_type(config::TYPES_PATH, &entry.param_type_name).await {
            ok = false;
            errors.push(json!({ "entity": entry.param_type_name, "error": e.to_string() }));
        }
        if let Some(result_type) = &entry.result_type_name {
            if let Err(e) = solx::delete_type(config::TYPES_PATH, result_type).await {
                ok = false;
                errors.push(json!({ "entity": result_type.clone(), "error": e.to_string() }));
            }
        }
        if ok {
            removed.push(entry.action_name.clone());
        }
    }

    let server_dir = home.join("tools").join(&server);
    if server_dir.exists() {
        std::fs::remove_dir_all(&server_dir)
            .map_err(|e| anyhow::anyhow!("failed to remove {}: {e}", server_dir.display()))?;
    }

    Ok(json!({ "server": server, "tools_removed": removed, "errors": errors }))
}

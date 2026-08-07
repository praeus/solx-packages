//! Load/save `mcp-servers.json` (user-managed server registry), per-server
//! `manifest.json` (source of truth for `solx-mcp remove`), and the small
//! per-tool `tool.json` descriptor that `solx-mcp invoke` reads from its cwd.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Resolve the solx-mcp-actions package home directory:
/// 1. Explicit CLI `--home` flag
/// 2. `SOLX_MCP_ACTIONS_HOME` env var
/// 3. Parent of the current executable (since the binary lives in `bin/`,
///    the package root is one level up)
pub fn resolve_home(explicit: Option<&str>) -> PathBuf {
    if let Some(dir) = explicit {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("SOLX_MCP_ACTIONS_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    // Binary is at <package>/bin/solx-mcp-actions[.exe]; package root is ../.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerDef {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServersConfig {
    #[serde(default)]
    pub servers: HashMap<String, ServerDef>,
}

pub fn servers_config_path(home: &Path) -> PathBuf {
    home.join("mcp-servers.json")
}

pub fn load_servers_config(home: &Path) -> anyhow::Result<ServersConfig> {
    let path = servers_config_path(home);
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read mcp-servers.json at {}: {e}",
            path.display()
        )
    })?;
    let cfg: ServersConfig = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("mcp-servers.json at {} is not valid JSON: {e}", path.display()))?;
    Ok(cfg)
}

pub fn find_server<'a>(cfg: &'a ServersConfig, name: &str) -> anyhow::Result<&'a ServerDef> {
    cfg.servers
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("server '{name}' is not configured in mcp-servers.json"))
}

/// Descriptor written into every per-tool directory. `solx-mcp invoke` reads
/// this relative to its own process cwd (set via the generated action's
/// `action_config.cwd`) to recover which server/tool it's supposed to call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub server: String,
    pub tool: String,
    /// Absolute path to `mcp-servers.json`, stored so `invoke` doesn't have
    /// to guess the package home directory from its (overridden) cwd.
    pub servers_config_path: String,
}

pub fn tool_dir(home: &Path, server: &str, tool_sanitized: &str) -> PathBuf {
    home.join("tools").join(server).join(tool_sanitized)
}

pub fn tool_descriptor_path(tool_dir: &Path) -> PathBuf {
    tool_dir.join("tool.json")
}

/// Write `tool.json` atomically (write to a `.tmp` sibling, then rename) so a
/// concurrent `invoke` can never observe a torn/partial write while a
/// re-import is in progress.
pub fn write_tool_descriptor(tool_dir: &Path, descriptor: &ToolDescriptor) -> anyhow::Result<()> {
    std::fs::create_dir_all(tool_dir)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", tool_dir.display()))?;
    let final_path = tool_descriptor_path(tool_dir);
    let tmp_path = tool_dir.join("tool.json.tmp");
    let body = serde_json::to_string_pretty(descriptor)?;
    std::fs::write(&tmp_path, body)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| anyhow::anyhow!("failed to rename {} -> {}: {e}", tmp_path.display(), final_path.display()))?;
    Ok(())
}

pub fn read_tool_descriptor(dir: &Path) -> anyhow::Result<ToolDescriptor> {
    let path = tool_descriptor_path(dir);
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        anyhow::anyhow!(
            "tool.json not found in {} ({e}); the per-tool directory may have been deleted \
             — re-run 'solx-mcp-actions import <server>'",
            dir.display()
        )
    })?;
    serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("tool.json at {} is not valid JSON: {e}", path.display()))
}

/// One imported tool, recorded in the per-server manifest — the source of
/// truth `solx-mcp remove` uses to know exactly which entities/directories it
/// created (deliberately not a name-prefix scan, which risks colliding with
/// unrelated user-created actions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub tool: String,
    pub action_name: String,
    pub param_type_name: String,
    pub dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub server: String,
    pub imported_at: String,
    #[serde(default)]
    pub tools: Vec<ManifestEntry>,
}

pub fn manifest_path(home: &Path, server: &str) -> PathBuf {
    home.join("tools").join(server).join("manifest.json")
}

pub fn load_manifest(home: &Path, server: &str) -> anyhow::Result<Manifest> {
    let path = manifest_path(home, server);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("no manifest for server '{server}' at {}: {e}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("manifest at {} is not valid JSON: {e}", path.display()))
}

pub fn write_manifest(home: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let path = manifest_path(home, &manifest.server);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
    }
    let tmp_path = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(manifest)?;
    std::fs::write(&tmp_path, body)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| anyhow::anyhow!("failed to rename {} -> {}: {e}", tmp_path.display(), path.display()))?;
    Ok(())
}

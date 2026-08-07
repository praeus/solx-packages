//! Talk to solx-server's HTTP API directly to create/delete solx Action and
//! Type entities at runtime. This avoids the tantivy lock conflict that
//! occurs when shelling out to `solx.exe` while the parent `solx exec`
//! already holds the docs index writer lock.
//!
//! Uses the same `server_token` from `solx-config.json` that `solx-cli`
//! uses when `SOLX_SERVER_URL` is set.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Resolve the server URL: `SOLX_SERVER_URL` env var, else default localhost.
fn server_url() -> String {
    std::env::var("SOLX_SERVER_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8766".to_string())
}

/// Read the server token from `solx-config.json` in the appdata dir.
fn server_token() -> anyhow::Result<String> {
    // Try SOLX_SERVER_TOKEN env var first.
    if let Ok(tok) = std::env::var("SOLX_SERVER_TOKEN") {
        if !tok.trim().is_empty() {
            return Ok(tok);
        }
    }
    // Fall back to reading solx-config.json.
    let appdata = solx_config_dir();
    let config_path = appdata.join("solx-config.json");
    let raw = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", config_path.display()))?;
    let cfg: Value = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", config_path.display()))?;
    cfg.get("server_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no server_token in solx-config.json"))
}

fn solx_config_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("SOLX_APPDATA_DIR") {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.trim().is_empty() {
                return std::path::PathBuf::from(appdata).join("praeus").join("solx");
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return std::path::PathBuf::from(home).join(".praeus").join("solx");
        }
    }
    std::env::temp_dir().join("praeus").join("solx")
}

#[derive(Serialize)]
struct PostRequest<T: Serialize> {
    path: String,
    name: String,
    input: T,
}

#[derive(Serialize)]
struct RefRequest {
    path: String,
    name: String,
}

#[derive(Deserialize)]
struct ServerError {
    error: String,
}

async fn post_json<Req: Serialize, Resp: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    route: &str,
    body: &Req,
) -> anyhow::Result<Resp> {
    let resp = client
        .post(format!("{url}{route}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("HTTP request to {route} failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| anyhow::anyhow!("read response: {e}"))?;
    if !status.is_success() {
        if let Ok(err) = serde_json::from_str::<ServerError>(&text) {
            anyhow::bail!("server {route}: {}", err.error);
        }
        anyhow::bail!("server {route} returned {status}: {text}");
    }
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("parse {route} response: {e} (body: {text})"))
}

/// Create or update a type via the server API.
pub async fn new_type(name: &str, body: &Value) -> anyhow::Result<Value> {
    let url = server_url();
    let token = server_token()?;
    let client = reqwest::Client::new();
    let req = PostRequest {
        path: "/packages/solx-mcp-actions".to_string(),
        name: name.to_string(),
        input: body.clone(),
    };
    let resp: Value = post_json(&client, &url, &token, "/types/post", &req).await?;
    Ok(resp)
}

/// Create or update an action via the server API.
pub async fn new_action(name: &str, body: &Value) -> anyhow::Result<Value> {
    let url = server_url();
    let token = server_token()?;
    let client = reqwest::Client::new();
    let req = PostRequest {
        path: "/".to_string(),
        name: name.to_string(),
        input: body.clone(),
    };
    let resp: Value = post_json(&client, &url, &token, "/actions/post", &req).await?;
    Ok(resp)
}

/// Delete an action via the server API.
pub async fn delete_action(name: &str) -> anyhow::Result<()> {
    let url = server_url();
    let token = server_token()?;
    let client = reqwest::Client::new();
    let req = RefRequest {
        path: "/".to_string(),
        name: name.to_string(),
    };
    let _: Value = post_json(&client, &url, &token, "/actions/delete", &req).await?;
    Ok(())
}

/// Delete a type via the server API.
pub async fn delete_type(name: &str) -> anyhow::Result<()> {
    let url = server_url();
    let token = server_token()?;
    let client = reqwest::Client::new();
    let req = RefRequest {
        path: "/packages/solx-mcp-actions".to_string(),
        name: name.to_string(),
    };
    let _: Value = post_json(&client, &url, &token, "/types/delete", &req).await?;
    Ok(())
}

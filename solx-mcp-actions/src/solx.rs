//! Talk to solx-server's HTTP API directly to create/delete solx Action and
//! Type entities at runtime. This avoids the tantivy lock conflict that
//! occurs when shelling out to `solx.exe` while the parent `solx exec`
//! already holds the docs index writer lock.
//!
//! Authenticates with `SOLX_SERVER_TOKEN`, which solx sets on every Command
//! action it spawns.
//!
//! solx-server's action/type routes are RESTful: `PUT /actions/{*ref}` and
//! `PUT /types/{*ref}` create-or-replace the entity at `ref` (its full
//! `path`+`name`, e.g. `/packages/solx-mcp-actions/firefox/mcp-firefox-hover-by-uid`),
//! with `ref` carrying the identity instead of a `{path,name,input}` body
//! wrapper. `DELETE` on the same URL removes it. There is no longer a
//! `POST /actions/save` — a bare `POST` on an action/type URL *executes* it
//! (see `solx-server/src/routes/actions.rs`), so getting this wrong doesn't
//! 404, it silently calls the wrong handler.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

/// Resolve the server URL: `SOLX_SERVER_URL` env var, else default localhost.
fn server_url() -> String {
    std::env::var("SOLX_SERVER_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8766".to_string())
}

/// The bearer token solx sets on every Command action it spawns.
fn server_token() -> anyhow::Result<String> {
    std::env::var("SOLX_SERVER_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "SOLX_SERVER_TOKEN is required; solx sets it when it runs this action"
            )
        })
}

#[derive(Deserialize)]
struct ServerError {
    error: String,
}

/// Build the `{*ref}` URL segment for an entity at `path`+`name`, matching
/// `solx_surface::path::full_ref` (root `/` needs no separator, anything
/// else does).
fn full_ref(path: &str, name: &str) -> String {
    if path == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", path.trim_end_matches('/'))
    }
}

async fn request_json<Req: serde::Serialize, Resp: DeserializeOwned>(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    route: &str,
    body: Option<&Req>,
) -> anyhow::Result<Resp> {
    let mut req = client.request(method, format!("{url}{route}")).bearer_auth(token);
    if let Some(body) = body {
        req = req.json(body);
    }
    let resp = req.send().await.map_err(|e| anyhow::anyhow!("HTTP request to {route} failed: {e}"))?;
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

/// Delete has no response body worth parsing (204 No Content on success).
async fn request_no_content<Req: serde::Serialize>(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    route: &str,
    body: Option<&Req>,
) -> anyhow::Result<()> {
    let mut req = client.request(method, format!("{url}{route}")).bearer_auth(token);
    if let Some(body) = body {
        req = req.json(body);
    }
    let resp = req.send().await.map_err(|e| anyhow::anyhow!("HTTP request to {route} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        if let Ok(err) = serde_json::from_str::<ServerError>(&text) {
            anyhow::bail!("server {route}: {}", err.error);
        }
        anyhow::bail!("server {route} returned {status}: {text}");
    }
    Ok(())
}

/// Create or update a type at `path`/`name` via the server API.
pub async fn new_type(path: &str, name: &str, body: &Value) -> anyhow::Result<Value> {
    let url = server_url();
    let token = server_token()?;
    let client = reqwest::Client::new();
    let route = format!("/types{}", full_ref(path, name));
    request_json(&client, reqwest::Method::PUT, &url, &token, &route, Some(body)).await
}

/// Create or update an action at `path`/`name` via the server API.
pub async fn new_action(path: &str, name: &str, body: &Value) -> anyhow::Result<Value> {
    let url = server_url();
    let token = server_token()?;
    let client = reqwest::Client::new();
    let route = format!("/actions{}", full_ref(path, name));
    request_json(&client, reqwest::Method::PUT, &url, &token, &route, Some(body)).await
}

/// Delete an action at `path`/`name` via the server API.
pub async fn delete_action(path: &str, name: &str) -> anyhow::Result<()> {
    let url = server_url();
    let token = server_token()?;
    let client = reqwest::Client::new();
    let route = format!("/actions{}", full_ref(path, name));
    request_no_content::<()>(&client, reqwest::Method::DELETE, &url, &token, &route, None).await
}

/// Delete a type at `path`/`name` via the server API.
pub async fn delete_type(path: &str, name: &str) -> anyhow::Result<()> {
    let url = server_url();
    let token = server_token()?;
    let client = reqwest::Client::new();
    let route = format!("/types{}", full_ref(path, name));
    request_no_content::<()>(&client, reqwest::Method::DELETE, &url, &token, &route, None).await
}

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use componentize_qjs::{componentize, ComponentizeOpts, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solx_package_log::server::ServerConfig;
use std::{fs, path::{Path, PathBuf}, time::Duration};
use tempfile::TempDir;

#[derive(Debug, Parser)]
#[command(name = "solx-quickjs")]
struct Args {
    #[arg(long = "action_name", alias = "action-name")]
    action_name: String,

    /// Path of the target action to create/update (e.g. `/packages/solx-quickjs`).
    #[arg(long = "path", alias = "action-path")]
    path: Option<String>,

    #[arg(long = "entry_artifact_name", alias = "entry-artifact-name")]
    entry_artifact_name: Option<String>,

    /// Inline JavaScript source. When set, this is compiled directly as the
    /// entry module (no file-store / disk read), and `entry_artifact_name` /
    /// `source_artifact_names` are optional.
    #[arg(long = "js_source", alias = "js-source")]
    js_source: Option<String>,

    #[arg(long = "source_artifact_names", alias = "source-artifact-names", value_delimiter = ',')]
    source_artifact_names: Vec<String>,

    #[arg(long = "output_artifact_name", alias = "output-artifact-name")]
    output_artifact_name: Option<String>,

    #[arg(long = "artifact_root", alias = "artifact-root")]
    artifact_root: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BuildResult {
    action_name: String,
    entry_artifact_name: String,
    output_artifact_name: String,
    wasm_bytes: usize,
}

/// Percent-encode a single URL path segment.
fn encode_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Percent-encode each segment of a `/`-separated rel-path, keeping the
/// separators and dropping any leading slash.
fn encode_rel_path(rel: &str) -> String {
    rel.split('/')
        .filter(|s| !s.is_empty())
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

async fn get_file(
    client: &reqwest::Client,
    cfg: &ServerConfig,
    rel_path: &str,
) -> Result<Vec<u8>, String> {
    let url = format!(
        "{}/files/{}",
        cfg.server_url.trim_end_matches('/'),
        encode_rel_path(rel_path)
    );
    let resp = client
        .get(&url)
        .bearer_auth(&cfg.server_token)
        .send()
        .await
        .map_err(|e| format!("GET {url} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!("GET {url} returned HTTP {status}: {body}"));
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("read {url} body: {e}"))
}

async fn put_file(
    client: &reqwest::Client,
    cfg: &ServerConfig,
    rel_path: &str,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let url = format!(
        "{}/files/{}",
        cfg.server_url.trim_end_matches('/'),
        encode_rel_path(rel_path)
    );
    let resp = client
        .put(&url)
        .bearer_auth(&cfg.server_token)
        .body(bytes)
        .send()
        .await
        .map_err(|e| format!("PUT {url} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!("PUT {url} returned HTTP {status}: {body}"));
    }
    Ok(())
}

async fn put_action(
    client: &reqwest::Client,
    cfg: &ServerConfig,
    path: &str,
    name: &str,
    body: &Value,
) -> Result<(), String> {
    let full = format!("{}/{}", path.trim_start_matches('/'), name);
    let url = format!(
        "{}/actions/{}",
        cfg.server_url.trim_end_matches('/'),
        encode_rel_path(&full)
    );
    let resp = client
        .put(&url)
        .bearer_auth(&cfg.server_token)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("PUT {url} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!("PUT {url} returned HTTP {status}: {body_text}"));
    }
    Ok(())
}

fn build_args_from_params_json(params_json: &str) -> Result<Args> {
    if params_json.trim().is_empty() {
        return Ok(Args::parse());
    }

    let params: Value = serde_json::from_str(params_json).context("parse stdin params as JSON")?;
    let action_name = params
        .get("action_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing action_name in stdin params"))?
        .to_string();
    let entry_artifact_name = params
        .get("entry_artifact_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let js_source = params
        .get("js_source")
        .and_then(Value::as_str)
        .map(str::to_string);
    let source_artifact_names = params
        .get("source_artifact_names")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Args {
        action_name,
        path: params
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string),
        entry_artifact_name,
        js_source,
        source_artifact_names,
        output_artifact_name: params
            .get("output_artifact_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        artifact_root: params
            .get("artifact_root")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_from_params_json() {
        let args = build_args_from_params_json(r#"{"action_name":"demo-js-action","entry_artifact_name":"main.js","source_artifact_names":["main.js"]}"#).unwrap();
        assert_eq!(args.action_name, "demo-js-action");
        assert_eq!(args.entry_artifact_name.as_deref(), Some("main.js"));
        assert_eq!(args.source_artifact_names, vec!["main.js"]);
    }

    #[test]
    fn parse_args_from_params_json_inline_source() {
        let args = build_args_from_params_json(r#"{"action_name":"demo-js-action","js_source":"export const runner = {}"}"#).unwrap();
        assert_eq!(args.action_name, "demo-js-action");
        assert_eq!(args.entry_artifact_name, None);
        assert_eq!(args.js_source.as_deref(), Some("export const runner = {}"));
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    solx_package_log::init("solx-quickjs");

    // `--file-only` is baked into the `build-javascript-file` command_actions
    // entry (see solx-quickjs/package.json), never into the JSON params — so
    // a caller can't opt back into upserting an action by shaping its
    // request. It's checked directly against argv rather than threaded
    // through `Args`/`build_args_from_params_json`, since those are built
    // from stdin JSON only and ignore real CLI args whenever stdin is
    // non-empty (the normal case when invoked as a Command action).
    let file_only = std::env::args().any(|a| a == "--file-only");

    use std::io::{IsTerminal, Read};
    let stdin_params = if std::io::stdin().is_terminal() {
        String::new()
    } else {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).context("read stdin")?;
        buf
    };
    let args = build_args_from_params_json(&stdin_params)?;

    // Resolve the entry source: inline `js_source` wins; otherwise a named
    // entry artifact is read from the file store (server mode) or local disk.
    let inline = args.js_source.is_some();
    let entry_name = args
        .entry_artifact_name
        .clone()
        .unwrap_or_else(|| if inline { "entry.js".to_string() } else { String::new() });
    if !inline && entry_name.is_empty() {
        return Err(anyhow!("missing entry_artifact_name (or js_source) in params"));
    }
    let output_artifact_name = args
        .output_artifact_name
        .clone()
        .unwrap_or_else(|| {
            if inline {
                format!("{}.wasm", args.action_name)
            } else {
                format!("{}.wasm", entry_name)
            }
        });

    // Two source-loading modes:
    //  - Server mode (default): read JS sources from the FileStore over HTTP
    //    and, after compiling, upload the wasm + upsert the target action.
    //  - Local mode: `artifact_root` is set (manual CLI invocation) — read
    //    sources from local disk and write the wasm back to disk, no HTTP.
    let server = if args.artifact_root.is_some() {
        None
    } else {
        Some(ServerConfig::from_env().map_err(anyhow::Error::msg)?)
    };
    let client = server
        .as_ref()
        .map(|_| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client")
        });

    let temp_dir = TempDir::new().context("create temp dir")?;
    let temp_path = temp_dir.path();

    // Stage the entry source into the temp dir.
    let entry_path = temp_path.join(&entry_name);
    if let Some(js) = &args.js_source {
        fs::write(&entry_path, js).context("write inline js source")?;
    } else {
        match (&server, &client) {
            (Some(cfg), Some(http)) => {
                let bytes = get_file(http, cfg, &entry_name).await.map_err(anyhow::Error::msg)?;
                fs::write(&entry_path, bytes).context("write entry artifact to temp")?;
            }
            _ => {
                let artifact_root = args.artifact_root.as_deref().unwrap_or_else(|| Path::new("."));
                let source_path = artifact_root.join(&entry_name);
                if !source_path.exists() {
                    return Err(anyhow!("entry artifact not found: {}", entry_name));
                }
                fs::copy(&source_path, &entry_path).context("copy entry artifact")?;
            }
        }
    }

    // Stage any additional source files (imports) into the temp dir.
    for source_name in &args.source_artifact_names {
        if source_name == &entry_name {
            continue;
        }
        let relative_name = Path::new(source_name)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid source artifact name: {}", source_name))?
            .to_string();
        let destination = temp_path.join(&relative_name);

        match (&server, &client) {
            (Some(cfg), Some(http)) => {
                let bytes = get_file(http, cfg, source_name).await.map_err(anyhow::Error::msg)?;
                fs::write(&destination, bytes).context("write source artifact to temp")?;
            }
            _ => {
                let artifact_root = args.artifact_root.as_deref().unwrap_or_else(|| Path::new("."));
                let source_path = artifact_root.join(source_name);
                if !source_path.exists() {
                    return Err(anyhow!("source artifact not found: {}", source_name));
                }
                fs::copy(&source_path, &destination).context("copy source artifact")?;
            }
        }
    }

    let wit_path = PathBuf::from(env!("CUSTOM_WIT"));
    if !wit_path.exists() {
        return Err(anyhow!("wit file not found: {}", wit_path.display()));
    }

    solx_package_log::info(&format!(
        "compiling action '{}': entry={}, sources={:?}",
        args.action_name, entry_name, args.source_artifact_names
    ))
    .await;

    let opts = ComponentizeOpts {
        wit_path: &wit_path,
        js_source: &std::fs::read_to_string(&entry_path).context("read entry artifact")?,
        js_path: Some(&entry_path),
        module_root: Some(temp_path),
        world_name: Some("custom-action"),
        stub_wasi: true,
        disable_gc: false,
        runtime: Runtime::OptSizeSync,
    };

    let wasm_bytes = componentize(&opts).await?;
    let wasm_len = wasm_bytes.len();

    // Persist the compiled wasm: upload to the FileStore (server mode) or
    // write to local disk (manual mode).
    let shared_rel = format!("files/actions/shared/{output_artifact_name}");
    match (&server, &client) {
        (Some(cfg), Some(http)) => {
            put_file(http, cfg, &shared_rel, wasm_bytes).await.map_err(anyhow::Error::msg)?;
        }
        _ => {
            let artifact_root = args.artifact_root.as_deref().unwrap_or_else(|| Path::new("."));
            let output_path = artifact_root.join(&output_artifact_name);
            fs::write(&output_path, wasm_bytes).context("write wasm artifact")?;
        }
    }

    // Upsert the target action so its `bin_name` points at the artifact we
    // just stored. Server mode only — in local mode the caller stages the
    // wasm and registers the action themselves (the old 3-step flow).
    // Skipped entirely in `--file-only` mode: the artifact is built and
    // uploaded, but no action row is touched, so a package's own
    // `install.solx` can `save action` against it directly.
    if !file_only {
        if let (Some(cfg), Some(http)) = (&server, &client) {
            let path = args
                .path
                .clone()
                .unwrap_or_else(|| "/packages/solx-quickjs".to_string());
            let body = json!({
                "actionType": "wasm",
                "binName": output_artifact_name,
            });
            put_action(http, cfg, &path, &args.action_name, &body).await.map_err(anyhow::Error::msg)?;
        }
    }

    let result = BuildResult {
        action_name: args.action_name,
        entry_artifact_name: entry_name,
        output_artifact_name: output_artifact_name.clone(),
        wasm_bytes: wasm_len,
    };

    solx_package_log::info(&format!(
        "compiled {output_artifact_name}: {} bytes",
        result.wasm_bytes
    ))
    .await;

    println!("{}", serde_json::to_string(&result).unwrap());
    Ok(())
}

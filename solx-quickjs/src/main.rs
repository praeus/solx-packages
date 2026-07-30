use anyhow::{anyhow, Context, Result};
use clap::Parser;
use componentize_qjs::{componentize, ComponentizeOpts, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::{Path, PathBuf}};
use tempfile::TempDir;

#[derive(Debug, Parser)]
#[command(name = "solx-quickjs")]
struct Args {
    #[arg(long = "action_name", alias = "action-name")]
    action_name: String,

    #[arg(long = "entry_artifact_name", alias = "entry-artifact-name")]
    entry_artifact_name: String,

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
        .ok_or_else(|| anyhow!("missing entry_artifact_name in stdin params"))?
        .to_string();
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
        entry_artifact_name,
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
        assert_eq!(args.entry_artifact_name, "main.js");
        assert_eq!(args.source_artifact_names, vec!["main.js"]);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    use std::io::{IsTerminal, Read};
    let stdin_params = if std::io::stdin().is_terminal() {
        String::new()
    } else {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).context("read stdin")?;
        buf
    };
    let args = build_args_from_params_json(&stdin_params)?;
    let artifact_root = args.artifact_root.unwrap_or_else(|| PathBuf::from("."));
    let output_artifact_name = args.output_artifact_name.unwrap_or_else(|| format!("{}.wasm", args.entry_artifact_name));

    let temp_dir = TempDir::new().context("create temp dir")?;
    let temp_path = temp_dir.path();

    let mut source_files = Vec::new();
    for source_name in &args.source_artifact_names {
        let source_path = artifact_root.join(source_name);
        if !source_path.exists() {
            return Err(anyhow!("source artifact not found: {}", source_name));
        }
        let relative_name = Path::new(source_name)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid source artifact name: {}", source_name))?;
        let destination = temp_path.join(relative_name);
        fs::copy(&source_path, &destination).context("copy source artifact")?;
        source_files.push((relative_name.to_string(), destination));
    }

    let entry_path = temp_path.join(Path::new(&args.entry_artifact_name).file_name().unwrap_or_default());
    if !entry_path.exists() {
        return Err(anyhow!("entry artifact not found: {}", args.entry_artifact_name));
    }

    let wit_path = PathBuf::from(env!("CUSTOM_WIT"));
    if !wit_path.exists() {
        return Err(anyhow!("wit file not found: {}", wit_path.display()));
    }

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
    let output_path = artifact_root.join(&output_artifact_name);
    fs::write(&output_path, wasm_bytes).context("write wasm artifact")?;

    let result = BuildResult {
        action_name: args.action_name,
        entry_artifact_name: args.entry_artifact_name,
        output_artifact_name: output_artifact_name.clone(),
        wasm_bytes: fs::metadata(&output_path)?.len() as usize,
    };

    println!("{}", serde_json::to_string(&result).unwrap());
    Ok(())
}

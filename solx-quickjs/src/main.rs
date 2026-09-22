use anyhow::{anyhow, Context, Result};
use clap::Parser;
use componentize_qjs::{componentize, ComponentizeOpts, Runtime};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solx_package_log::server::ServerConfig;
use std::{fs, path::{Component as PathComponent, Path, PathBuf}, time::Duration};
use std::sync::LazyLock;
use tempfile::TempDir;

/// Confirm the entry source actually exports what the host will call, before
/// any time is spent compiling it.
///
/// `custom-action.wit`'s `runner` export is an *interface* export (`export
/// runner;`, where `interface runner { run: func(...) }`), and componentize-qjs
/// binds an interface export to a JS export **named after the interface** --
/// `export const runner = { run(actionName, params) { ... } }`. A bare
/// top-level `export function run(...)` (or `main`) does not bind to it.
///
/// **This has to be a source check, not a check of the compiled output --**
/// that was the first attempt, and it does not work. `componentize-qjs` uses
/// `wit-dylib` (see its README), which generates a wasm-level export
/// trampoline for *every* export the WIT world declares, unconditionally,
/// regardless of whether the JS actually backs it -- that is the whole point
/// of the "dylib" pattern, deferred binding resolved against the JS engine's
/// state at call time, not compile time. Compiling `export function main() {}`
/// against `custom-action.wit` and inspecting the result with
/// `wasmtime::component::Component::new(...).component_type()` shows
/// `sol:actions/runner@0.1.0#run` present and well-typed regardless -- the
/// missing binding only surfaces once something actually calls it, as the
/// same opaque `wasm \`unreachable\`` trap this check exists to prevent. A
/// structural check of the output cannot see the difference; only the source
/// can.
///
/// This is why the failure was opaque in the first place: the build has
/// nothing to report at build time under either approach *except* a source
/// check. A model (or a person) debugging the runtime trap alone has no way
/// to tell "wrong export shape" apart from "bug in my logic," and will
/// rebuild the same mistake indefinitely -- which is exactly what happened
/// the first time this was tried by an agent session, across dozens of
/// iterations and four rewrites, each one a smaller stub than the last,
/// because every one of them was missing the same thing.
///
/// The check is deliberately narrow: does the source contain, anywhere, an
/// `export` that binds the identifier `runner` (`export const/let/var
/// runner`, `export function runner`, or `export { ... runner ... }`,
/// covering a re-export from another staged file). It does not parse
/// JavaScript, so a `runner` that only appears in a comment or a string would
/// fool it, and an unconventional binding (`globalThis.runner = ...`,
/// dynamic `export`) would too -- but every real mistake observed so far
/// (`main`, bare `run`, no export at all) has none of those, and a false
/// negative here still just means the trap comes back opaque, the same
/// experience as before this check existed. A false positive is not possible
/// with only this pattern's inputs, since the check is conservative for what
/// it accepts, not what it rejects.
static RUNNER_EXPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*export\s+(?:(?:const|let|var|function)\s+runner\b|\{[^}]*\brunner\b[^}]*\})")
        .expect("RUNNER_EXPORT pattern is a fixed, tested literal")
});

fn verify_runner_export(js_source: &str) -> Result<()> {
    if RUNNER_EXPORT.is_match(js_source) {
        return Ok(());
    }

    Err(anyhow!(
        "no `export ... runner` found in the entry source -- this would build \
         without error and only fail later, at run time, with an opaque wasm \
         trap that names nothing wrong. Your JavaScript must export a named \
         `runner` object, not a bare top-level function:\n\n\
         export const runner = {{\n  run(actionName, params) {{\n    \
         return {{ success: true, message: null, output: JSON.stringify(result) }};\n  \
         }}\n}};\n\n\
         `export function run(...)` or `export function main(...)` builds fine \
         but never actually binds to the required interface, so it will never run."
    ))
}

#[derive(Debug, Parser)]
#[command(name = "solx-quickjs")]
struct Args {
    /// Only meaningful when an action row is being upserted. In `--file-only`
    /// mode nothing consumes it, so it is optional there; `main` enforces it
    /// for the upserting path.
    #[arg(long = "action_name", alias = "action-name")]
    action_name: Option<String>,

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
    action_name: Option<String>,
    entry_artifact_name: String,
    output_artifact_name: String,
    wasm_bytes: usize,
    /// `FileRef`-shaped entries (`name`/`relPath`/`contentType`) for the JS
    /// source(s) and the compiled wasm, ready to splice into a `save action`
    /// or `entity-save-action` `files` field, e.g.
    /// `"files":$build.result.files` in the caller's `.solx` script.
    files: Vec<Value>,
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

/// Where a staged source lands inside the temp module root.
///
/// The artifact name is treated as a *relative path*, not just a filename, so
/// a package can stage `vendor/lib.js` and import it as `./vendor/lib.js`.
/// Flattening to the basename -- which is what this used to do -- collides the
/// moment two directories hold the same filename, and it is what stopped a
/// multi-file library from ever resolving: componentize-qjs runs a real node
/// resolver over the module root, so the only thing missing was the tree.
///
/// The name arrives in a params payload, so it is checked rather than trusted.
/// Only ordinary path components are allowed: no root, no prefix, no `..`, so
/// a staged file cannot land outside the module root. The resolver enforces
/// the same boundary when it loads, but a build should fail at staging with a
/// clear message rather than later with a resolver error.
fn staged_destination(root: &Path, source_name: &str) -> Result<PathBuf> {
    let relative = Path::new(source_name);
    if relative.as_os_str().is_empty() {
        return Err(anyhow!("empty source artifact name"));
    }
    for component in relative.components() {
        if !matches!(component, PathComponent::Normal(_)) {
            return Err(anyhow!(
                "source artifact name must be a relative path with no '..' or root: {source_name}"
            ));
        }
    }
    Ok(root.join(relative))
}

/// Create the parent directory of a staged file, if it has one.
fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create staging directory {}", parent.display()))?;
    }
    Ok(())
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
    // Not required here: `--file-only` builds never touch an action row and so
    // have no use for a name. `main` rejects a missing one on the path that
    // actually needs it.
    let action_name = params
        .get("action_name")
        .and_then(Value::as_str)
        .map(str::to_string);
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
    fn accepts_the_documented_runner_shape() {
        verify_runner_export(
            "export const runner = {\n  run(actionName, params) {\n    return {};\n  }\n};\n",
        )
        .expect("the one shape every skill and sample file teaches must pass");
    }

    #[test]
    fn accepts_let_and_var_and_a_bare_function_named_runner() {
        for src in [
            "export let runner = { run() {} };",
            "export var runner = { run() {} };",
            "export function runner() {}",
        ] {
            verify_runner_export(src).unwrap_or_else(|e| panic!("{src:?} should pass: {e}"));
        }
    }

    #[test]
    fn accepts_a_re_export_of_runner_from_another_staged_file() {
        // Multi-file libraries stage more than one source; `runner` can live
        // in one of them and be re-exported from the entry file.
        verify_runner_export("export { runner } from './impl.js';")
            .expect("a re-export naming runner must pass");
        verify_runner_export("export { makeRunner as runner };")
            .expect("an aliased export must pass");
    }

    #[test]
    fn rejects_the_mistake_actually_made_by_a_live_agent_session() {
        // Verbatim (trimmed) from the four rewrites in
        // /agent/sessions/upland-bramble and hidden-antler: each one defined
        // `main`, never `runner`, and each one built without a single error.
        for src in [
            "export function main(input) {\n  return { received: typeof input };\n}\n",
            "export function main() {\n  return { ok: true };\n}\n",
            "function main() {\n  return { ok: true };\n}\n",
            "\n",
        ] {
            let err = verify_runner_export(src)
                .expect_err(&format!("{src:?} has no runner export and must be rejected"));
            let msg = err.to_string();
            assert!(msg.contains("export const runner"), "{msg}");
        }
    }

    #[test]
    fn a_bare_run_export_is_the_other_documented_mistake_and_is_also_rejected() {
        assert!(verify_runner_export("export function run(actionName, params) {}").is_err());
    }

    #[test]
    fn a_single_line_comment_or_a_string_literal_does_not_count() {
        // These read as ordinary lines to the eye but do not start with
        // `export` once leading whitespace is stripped, so the regex
        // (correctly, if incidentally) still misses them.
        for src in [
            "// export const runner = { run() {} };\nexport function main() {}",
            "const note = 'export const runner';\nexport function main() {}",
        ] {
            assert!(verify_runner_export(src).is_err(), "{src:?}: still just main()");
        }
    }

    #[test]
    fn a_block_commented_export_line_is_a_known_false_positive() {
        // The gap the function doc warns about, made concrete: `(?m)^` has no
        // notion of `/* */` block-comment context, so a line that merely
        // *looks* like the real export -- even one a person left commented
        // out while debugging -- still matches. Asserted here, rather than
        // left as a claim in a comment, so a future change to the regex that
        // silently closes (or widens) this gap gets noticed either way.
        let src = "/*\nexport const runner = { run() {} };\n*/\nexport function main() {}";
        assert!(
            verify_runner_export(src).is_ok(),
            "known gap: a commented-out export line still matches"
        );
    }

    #[test]
    fn a_nested_source_name_keeps_its_directory() {
        // The whole point: componentize-qjs resolves imports with a real node
        // resolver over the module root, so a library staged as several files
        // only works if the tree survives staging.
        let root = Path::new("/tmp/build");
        assert_eq!(
            staged_destination(root, "vendor/lib/index.js").unwrap(),
            root.join("vendor").join("lib").join("index.js")
        );
        assert_eq!(staged_destination(root, "main.js").unwrap(), root.join("main.js"));
    }

    #[test]
    fn a_source_name_cannot_escape_the_module_root() {
        let root = Path::new("/tmp/build");
        for bad in ["../secrets.js", "vendor/../../secrets.js", "/etc/passwd", ""] {
            assert!(
                staged_destination(root, bad).is_err(),
                "{bad:?} must be refused before anything is written"
            );
        }
    }

    #[test]
    fn parse_args_from_params_json() {
        let args = build_args_from_params_json(r#"{"action_name":"demo-js-action","entry_artifact_name":"main.js","source_artifact_names":["main.js"]}"#).unwrap();
        assert_eq!(args.action_name.as_deref(), Some("demo-js-action"));
        assert_eq!(args.entry_artifact_name.as_deref(), Some("main.js"));
        assert_eq!(args.source_artifact_names, vec!["main.js"]);
    }

    #[test]
    fn parse_args_without_an_action_name() {
        // What `build-javascript-file` sends: no action row is being touched,
        // so there is no name to send. Parsing must not reject it -- `main`
        // enforces the name only on the upserting path.
        let args = build_args_from_params_json(
            r#"{"entry_artifact_name":"solx-conductor.js","source_artifact_names":["solx-conductor.js","names.js"],"output_artifact_name":"solx-conductor.wasm"}"#,
        )
        .unwrap();
        assert_eq!(args.action_name, None);
        assert_eq!(args.entry_artifact_name.as_deref(), Some("solx-conductor.js"));
        assert_eq!(args.output_artifact_name.as_deref(), Some("solx-conductor.wasm"));
    }

    #[test]
    fn parse_args_from_params_json_inline_source() {
        let args = build_args_from_params_json(r#"{"action_name":"demo-js-action","js_source":"export const runner = {}"}"#).unwrap();
        assert_eq!(args.action_name.as_deref(), Some("demo-js-action"));
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

    // Only the upserting path needs a name to put the action under.
    if !file_only && args.action_name.is_none() {
        return Err(anyhow!("missing action_name in stdin params"));
    }

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
    // An inline build has no entry filename to derive an output name from, so
    // it falls back to the action name -- which `--file-only` need not supply.
    // When neither is present there is nothing to name the artifact after, and
    // guessing would silently overwrite whatever shares the guess.
    let output_artifact_name = match args.output_artifact_name.clone() {
        Some(name) => name,
        None if inline => match &args.action_name {
            Some(name) => format!("{name}.wasm"),
            None => {
                return Err(anyhow!(
                    "missing output_artifact_name: an inline js_source build with no action_name has nothing to name the artifact after"
                ))
            }
        },
        None => format!("{}.wasm", entry_name),
    };

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
    let entry_path = if inline { temp_path.join(&entry_name) } else { staged_destination(temp_path, &entry_name)? };
    ensure_parent(&entry_path)?;
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
        let destination = staged_destination(temp_path, source_name)?;
        ensure_parent(&destination)?;

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
        args.action_name.as_deref().unwrap_or(&output_artifact_name),
        entry_name,
        args.source_artifact_names
    ))
    .await;

    let entry_source = std::fs::read_to_string(&entry_path).context("read entry artifact")?;

    // Catch a wrong export shape before any time is spent compiling it -- see
    // `verify_runner_export` for why this has to run on the source, not the
    // compiled result. Only the entry file is checked: a `runner` that only
    // exists behind a wildcard re-export (`export * from './impl.js'`) would
    // read as missing here, but every real build seen so far -- including the
    // one this check exists because of -- defines `runner` directly in the
    // entry file.
    verify_runner_export(&entry_source)?;

    let opts = ComponentizeOpts {
        wit_path: &wit_path,
        js_source: &entry_source,
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

    // `files` metadata for the "Attached files" UI: the JS source(s) at
    // whatever rel_path the caller already staged them under (unchanged --
    // this only describes where the bytes already live, it doesn't move
    // them), plus the wasm we just wrote. Skipped for the source entries on
    // an inline `js_source` build: there's no persisted rel_path to point at
    // in that case. Returned in `BuildResult` so callers of
    // `build-javascript-file` (which never touches an action row itself) can
    // splice it into their own `save action` via `$build.result.files`,
    // rather than every package hand-duplicating this array.
    let mut files: Vec<Value> = Vec::new();
    if !inline {
        let mut js_rel_paths: Vec<String> = vec![entry_name.clone()];
        for name in &args.source_artifact_names {
            if !js_rel_paths.contains(name) {
                js_rel_paths.push(name.clone());
            }
        }
        for rel_path in js_rel_paths {
            let name = Path::new(&rel_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&rel_path)
                .to_string();
            files.push(json!({
                "name": name,
                "relPath": rel_path,
                "contentType": "text/javascript",
            }));
        }
    }
    files.push(json!({
        "name": output_artifact_name,
        "relPath": shared_rel,
        "contentType": "application/wasm",
    }));

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
                "files": files,
            });
            // Checked above: `!file_only` guarantees a name.
            let action_name = args
                .action_name
                .as_deref()
                .expect("action_name is required when not in --file-only mode");
            put_action(http, cfg, &path, action_name, &body).await.map_err(anyhow::Error::msg)?;
        }
    }

    let result = BuildResult {
        action_name: args.action_name,
        entry_artifact_name: entry_name,
        output_artifact_name: output_artifact_name.clone(),
        wasm_bytes: wasm_len,
        files,
    };

    solx_package_log::info(&format!(
        "compiled {output_artifact_name}: {} bytes",
        result.wasm_bytes
    ))
    .await;

    println!("{}", serde_json::to_string(&result).unwrap());
    Ok(())
}

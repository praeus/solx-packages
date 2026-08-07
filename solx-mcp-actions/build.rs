use std::env;
use std::path::PathBuf;
use std::process::Command;

const INNER_BUILD_ENV: &str = "SOLX_MCP_ACTIONS_INNER_BUILD";
const SKIP_AUTOBUILD_ENV: &str = "SOLX_MCP_ACTIONS_SKIP_AUTOBUILD";

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var_os(INNER_BUILD_ENV).is_some() {
        return;
    }
    if env::var_os(SKIP_AUTOBUILD_ENV).is_some() {
        println!(
            "cargo:warning=Skipping solx-mcp auto-build because {} is set",
            SKIP_AUTOBUILD_ENV
        );
        return;
    }

    // Only stage on --release so dev/check is fast.
    if env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }

    if let Err(err) = build_and_stage() {
        println!("cargo:warning=solx-mcp-actions staging failed: {err}");
    }
}

fn build_and_stage() -> Result<(), String> {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR")
            .map_err(|e| format!("missing CARGO_MANIFEST_DIR: {e}"))?,
    );
    let manifest_path = manifest_dir.join("Cargo.toml");

    // Use a package-local target dir to avoid polluting the workspace target/.
    let package_target_dir = manifest_dir.join(".build-target");

    let status = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--target-dir")
        .arg(&package_target_dir)
        .arg("--manifest-path")
        .arg(&manifest_path)
        .env(INNER_BUILD_ENV, "1")
        .status()
        .map_err(|e| format!("failed to run nested cargo build: {e}"))?;

    if !status.success() {
        return Err("nested cargo build failed".to_string());
    }

    let bin_name = if cfg!(windows) { "solx-mcp-actions.exe" } else { "solx-mcp-actions" };

    let src = package_target_dir.join("release").join(bin_name);
    if !src.exists() {
        return Err(format!(
            "could not find built binary '{}' at {}",
            bin_name,
            src.display()
        ));
    }

    // Stage to <package>/bin/ — a stable, machine-independent path.
    let staged_dir = manifest_dir.join("bin");
    std::fs::create_dir_all(&staged_dir)
        .map_err(|e| format!("failed to create staging dir {}: {e}", staged_dir.display()))?;
    let staged = staged_dir.join(bin_name);
    std::fs::copy(&src, &staged)
        .map_err(|e| format!("failed to copy {} -> {}: {e}", src.display(), staged.display()))?;

    // Pure Rust — no native DLLs / shared libraries to stage.

    println!("cargo:warning=staged solx-mcp binary at {}", staged.display());
    Ok(())
}

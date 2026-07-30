use std::env;
use std::path::PathBuf;

fn main() {
    // Re-run if the upstream solx-custom-actions-lib build changes.
    println!("cargo:rerun-if-env-changed=DEP_SOL_CUSTOM_WIT_CUSTOM_WIT");
    println!("cargo:rerun-if-changed=build.rs");

    // solx-custom-actions-lib's build script emits CUSTOM_WIT (via its
    // `links = "sol_custom_wit"`) pointing at a copy of its canonical
    // `wit/custom-action.wit`. Forward it to the main crate as a rustc-env
    // so main.rs can find the file.
    let wit_path = env::var("DEP_SOL_CUSTOM_WIT_CUSTOM_WIT").unwrap_or_else(|_| {
        // Fallback: look for the canonical WIT in the sibling solx-core
        // checkout. Useful for local development outside the usual
        // cargo build path resolution.
        let manifest_dir = PathBuf::from(
            env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"),
        );
        let candidate = manifest_dir
            .join("..")
            .join("..")
            .join("solx-core")
            .join("solx-wasm")
            .join("wit")
            .join("custom-action.wit");
        candidate.display().to_string()
    });

    println!("cargo:rustc-env=CUSTOM_WIT={wit_path}");
}

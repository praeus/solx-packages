// Copies the canonical `custom-action.wit` from the solx-core workspace
// into this crate's local `wit/` directory so wit-bindgen can find it.
//
// Layout assumed (this crate is at solx-packages/solx-google/actions/solx-google-actions/):
//
//   solx-packages/solx-google/actions/solx-google-actions/  (this crate)
//   solx-core/solx-wasm/wit/custom-action.wit                (target)
//
// i.e. solx-core is expected to be a sibling of solx-packages.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wit_dst = manifest_dir.join("wit");
    std::fs::create_dir_all(&wit_dst).unwrap();

    // manifest_dir = .../solx-packages/solx-google/actions/solx-google-actions
    // Walk up three levels to solx-packages/, then across to solx-core/.
    let wit_src = manifest_dir
        .join("..").join("..").join("..")
        .join("..")
        .join("solx-core")
        .join("solx-wasm")
        .join("wit")
        .join("custom-action.wit");

    let dst = wit_dst.join("custom-action.wit");
    std::fs::copy(&wit_src, &dst).unwrap_or_else(|e| {
        panic!(
            "failed to copy custom-action.wit from {} to {}: {e}\n\
             hint: this crate expects the solx-core workspace to be a sibling \
             of solx-packages (solx-core/solx-wasm/wit/custom-action.wit). \
             Adjust the path in build.rs if your checkout is laid out differently.",
            wit_src.display(),
            dst.display()
        )
    });
    println!("cargo:rerun-if-changed={}", wit_src.display());
    println!("cargo:rerun-if-changed={}", dst.display());
}
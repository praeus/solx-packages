use std::env;
use std::path::PathBuf;
use std::process::Command;

const INNER_BUILD_ENV: &str = "SOLX_OMNIPARSE_INNER_BUILD";
const SKIP_AUTOBUILD_ENV: &str = "SOLX_OMNIPARSE_SKIP_AUTOBUILD";
const SKIP_MODEL_FETCH_ENV: &str = "SOLX_OMNIPARSE_SKIP_MODEL_FETCH";

/// Upstream `omniparse` ML OCR model descriptors. URLs and SHA-256s are
/// pinned in `omniparse/src/ocr/ml.rs` (`MODELS` const) and must be kept in
/// sync with the upstream release this package was built against.
struct ModelSpec {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
}

const MODELS: &[ModelSpec] = &[
    ModelSpec {
        name: "text-detection.rten",
        url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten",
        sha256: "f15cfb56bd02c4bf478a20343986504a1f01e1665c2b3a0ad66340f054b1b5ca",
    },
    ModelSpec {
        name: "text-recognition.rten",
        url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten",
        sha256: "e484866d4cce403175bd8d00b128feb08ab42e208de30e42cd9889d8f1735a6e",
    },
];

fn main() {
    println!("cargo:rerun-if-changed=src/main.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var_os(INNER_BUILD_ENV).is_some() {
        return;
    }
    if env::var_os(SKIP_AUTOBUILD_ENV).is_some() {
        println!(
            "cargo:warning=Skipping solx-omniparse auto-build because {} is set",
            SKIP_AUTOBUILD_ENV
        );
        return;
    }

    // Mirror sol-extractous / sol-gws: only stage on --release so dev/check is fast.
    if env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }

    if let Err(err) = build_and_stage() {
        println!("cargo:warning=solx-omniparse staging failed: {err}");
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

    // Cargo's default binary name = package name (`solx-omniparse-preprocess`).
    let bin_name = if cfg!(windows) {
        "solx-omniparse-preprocess.exe"
    } else {
        "solx-omniparse-preprocess"
    };

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

    println!(
        "cargo:warning=staged solx-omniparse binary at {}",
        staged.display()
    );

    // omniparse is pure Rust — no native DLLs / shared libraries to stage
    // (unlike sol-extractous which bundles Tika/JVM native assets).

    // Pre-fetch the ML OCR models so the install is self-contained.
    // These are ~12 MB total and would otherwise be lazy-downloaded on
    // first OCR call (with a 5–30 s cold-start + internet requirement).
    // Skip with `SOLX_OMNIPARSE_SKIP_MODEL_FETCH=1` for offline builds; the
    // binary will then fall back to upstream's default cache.
    if env::var_os(SKIP_MODEL_FETCH_ENV).is_some() {
        println!(
            "cargo:warning=Skipping solx-omniparse OCR model fetch because {} is set",
            SKIP_MODEL_FETCH_ENV
        );
    } else if let Err(err) = fetch_and_stage_models(&staged_dir) {
        println!("cargo:warning=solx-omniparse model staging failed: {err}");
    }

    Ok(())
}

/// Download each pinned ML OCR model into `<staged_dir>/models/`, verify the
/// SHA-256 against the upstream pinned hash, and report byte counts. Hash
/// mismatches are non-fatal: the partial file is removed and the binary
/// will lazy-download on first OCR use as a last-resort fallback.
fn fetch_and_stage_models(staged_dir: &std::path::Path) -> Result<(), String> {
    let models_dir = staged_dir.join("models");
    std::fs::create_dir_all(&models_dir)
        .map_err(|e| format!("failed to create models dir {}: {e}", models_dir.display()))?;

    let mut total_bytes: u64 = 0;
    let mut staged_count = 0usize;
    for spec in MODELS {
        let dest = models_dir.join(spec.name);
        // Skip the download if the file is already present and matches the
        // pinned hash. This makes incremental builds cheap.
        if dest.exists() {
            match sha256_file(&dest) {
                Ok(actual) if actual.eq_ignore_ascii_case(spec.sha256) => {
                    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                    total_bytes += size;
                    staged_count += 1;
                    println!(
                        "cargo:warning=ocr model {} already present ({} bytes, sha256 ok)",
                        spec.name, size
                    );
                    continue;
                }
                Ok(actual) => {
                    println!(
                        "cargo:warning=ocr model {} sha256 mismatch (expected {}, got {}); re-downloading",
                        spec.name, spec.sha256, actual
                    );
                    let _ = std::fs::remove_file(&dest);
                }
                Err(e) => {
                    println!(
                        "cargo:warning=ocr model {} present but unreadable ({}); re-downloading",
                        spec.name, e
                    );
                    let _ = std::fs::remove_file(&dest);
                }
            }
        }

        // Use `curl` for portability: Windows 10+, macOS, and all major
        // Linux distros ship `curl` with a stable `-fsSL` flag set.
        // `-f` fails on HTTP >= 400, `-s` silences progress, `-S` keeps
        // errors, `-L` follows redirects (the S3-accelerate URL 302-redirects).
        let status = Command::new("curl")
            .arg("-fsSL")
            .arg("-o")
            .arg(&dest)
            .arg(spec.url)
            .status()
            .map_err(|e| {
                format!(
                    "failed to invoke `curl` to fetch {} (is curl on PATH?): {e}",
                    spec.name
                )
            })?;

        if !status.success() {
            let _ = std::fs::remove_file(&dest);
            return Err(format!(
                "curl exited with {:?} while downloading {} from {}",
                status.code(),
                spec.name,
                spec.url
            ));
        }

        let actual = sha256_file(&dest).map_err(|e| {
            format!("failed to hash downloaded model {}: {e}", spec.name)
        })?;
        if !actual.eq_ignore_ascii_case(spec.sha256) {
            let _ = std::fs::remove_file(&dest);
            return Err(format!(
                "ocr model {} hash mismatch after download (expected {}, got {}); \
                 the upstream omniparse release may have rotated the model — \
                 lazy-download will be used at runtime",
                spec.name, spec.sha256, actual
            ));
        }

        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        total_bytes += size;
        staged_count += 1;
        println!(
            "cargo:warning=fetched ocr model {} ({} bytes, sha256 ok)",
            spec.name, size
        );
    }

    println!(
        "cargo:warning=staged {} ocr model(s) ({} bytes total) at {}",
        staged_count,
        total_bytes,
        models_dir.display()
    );
    Ok(())
}

/// Compute the SHA-256 of a file and return it as a lowercase hex string.
fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    // Hand-roll SHA-256 with a small fixed-size buffer to avoid pulling in
    // a crypto dep just for the build script. The algorithm is FIPS 180-4
    // and is short enough to keep here verbatim.
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut buf = [0u8; 64 * 1024];
    let mut total_len: u64 = 0;
    let mut tail = Vec::with_capacity(128);

    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        total_len = total_len.wrapping_add(n as u64);
        for chunk in buf[..n].chunks(64) {
            if chunk.len() == 64 {
                sha256_compress(&mut state, chunk);
            } else {
                tail.extend_from_slice(chunk);
            }
        }
    }

    // Padding: append 0x80, then zeros, then 64-bit big-endian length-in-bits.
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    let bit_len = total_len.wrapping_mul(8);
    tail.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in tail.chunks(64) {
        debug_assert_eq!(chunk.len(), 64);
        sha256_compress(&mut state, chunk);
    }

    let mut out = String::with_capacity(64);
    for word in state.iter() {
        out.push_str(&format!("{:08x}", word));
    }
    Ok(out)
}

#[inline]
fn sha256_compress(state: &mut [u32; 8], block: &[u8]) {
    debug_assert_eq!(block.len(), 64);
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
        0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    let mut f = state[5];
    let mut g = state[6];
    let mut h = state[7];
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}
# solx-omniparse

Omniparse-based extraction preprocess package for solx-core, with OCR support
for scanned and image-only PDFs.

This package provides one `Command` action:

`solx-omniparse-process-file-write` — extracts text, then POSTs the result to
solx-server's `PUT /docs/{path}/{name}` itself and returns
`{saved: [{path, name}], result: {...}}`. Requires `SOLX_SERVER_URL` +
`SOLX_SERVER_TOKEN` env vars; soft-fails to the raw `result` (with an empty
`saved` list) if the server is unreachable — this is what a user/model would
actually invoke, so the raw-only variant that returned text without saving
anywhere isn't registered as a separate action.

Unlike old sol, solx-core has no extraction pipeline or event-hook system, so
this action does not auto-run on anything — invoke it explicitly:

```bash
solx exec /packages/solx-omniparse/solx-omniparse-process-file-write --json '{"source_path":"...","file_name":"...","mime_type":"..."}'
```

## Install

`solx install-package` registers the action via `install.solx`. solx-core
has **no grant/approval step** for `Command` actions — `fn_name` is the
literal command solx-core runs, and it becomes executable the moment
`install.solx` posts it. Before installing, build the binary
(`cargo build --release` in this directory) and edit `install.solx`'s
`action_config.cwd` to the absolute path of this package's `bin/` directory
(there is no path templating in `.solx` scripts), or `solx save action` the
same reference again afterward to correct it.

## What it does

- Reads a JSON payload on stdin:
  - `source_path`
  - `file_name`
  - `mime_type`
- Applies to PDFs and common office-like formats (DOCX, XLSX, PPTX, ODT, ODS,
  ODP, EPUB, RTF, DOC, XLS, PPT).
- Uses the [`omniparse`](https://github.com/sirhco/omniparse) Rust library to
  extract text from the source file.
- **OCR fallback**: When a PDF's text layer is empty (scanned / image-only),
  omniparse automatically OCRs embedded JPEG images. The ML OCR backend
  (`ocrs` + `rten`) is enabled by default — models auto-download on first use
  (~12 MB). Override with `OMNIPARSE_OCR=classical` (pure Rust, no downloads)
  or `OMNIPARSE_OCR=off` (disable OCR).
- Returns JSON with:
  - `bytes_base64` containing UTF-8 plain text bytes
  - `file_name` rewritten to `*.txt`
  - `mime_type` rewritten to `text/plain`
- Returns `{}` when not applicable or conversion fails (fail-open behavior).

## Runtime model

This package runs entirely as a local Rust binary and does not require a
separate OmniParse Docker container or server process.

The `omniparse` crate used here is a local library dependency, not the
Python-based `adithya-s-k/omniparse` server repository. That upstream project
documents a Docker-hosted API server, but this package executes extraction
inline in the command action.

If you want to use the ML OCR backend, the only runtime requirement is the
optionally downloaded model data for `OMNIPARSE_OCR=ml`. No Docker compose or
external container orchestration is needed for `solx-omniparse`.

## OCR configuration

The `OMNIPARSE_OCR` environment variable controls which OCR backend runs:

| Value | Backend | Notes |
|-------|---------|-------|
| `ml` (default) | ocrs + rten ML models | Recommended for photos, screenshots, scanned PDFs. Models are pre-fetched into `bin/models/` at build time and auto-loaded on first run (no network needed). |
| `classical` | Pure-Rust classical pipeline | No downloads. Good only on clean printed scans with matched fonts. |
| `off` / unset | OCR disabled | Image parsers extract metadata only. |

This binary sets `OMNIPARSE_OCR=ml` by default if the variable is not already
set. To use a different backend, set `OMNIPARSE_OCR` in the shell that
launches `solx` before invoking the action (there is no `action_config.env`
support in solx-core).

For air-gapped / offline environments, pre-download models on a connected
machine and point `OMNIPARSE_OCR_MODELS` at the cached model directory.

See the [omniparse OCR guide](https://github.com/sirhco/omniparse/blob/main/OCR_GUIDE.md)
for full details on backend selection, model management, tuning, and debugging.

## PDF parsing

Omniparse uses a four-tier PDF fallback chain:

1. **Strict** — `lopdf` xref / trailer / object dictionary parsing
2. **Repair** — trailing-junk repair for truncated / malformed PDFs
3. **Raw scan** — stream-byte scan with FlateDecode / LZWDecode / ASCII85Decode
4. **pdf-extract** — linearized / Identity-H + /ToUnicode CMap PDFs (enabled via
   the `pdf-extract` feature, which this package includes)

The `pdf_parse_strategy` metadata field surfaces which tier ran. This handles
real-world PDFs from Lucidchart, Word print-to-PDF, browser print-to-PDF, and
truncated downloads — all yield text instead of `"Invalid file trailer"`.

## Build

The package includes a `build.rs` that, on `cargo build --release`, compiles
the binary into a package-local `.build-target/` and stages it into
`<package>/bin/`. The workspace-root `target/` directory is **not** used.
This package has no native DLLs to stage — omniparse is pure Rust.

From the package directory:

```bash
cargo build --release
```

To skip auto-staging (for example when you only want to run `cargo check`):

```bash
SOLX_OMNIPARSE_SKIP_AUTOBUILD=1 cargo build --release
```

To skip OCR-model pre-fetching (offline builds; the binary will fall back to
omniparse's default cache and lazy-download on first OCR call):

```bash
SOLX_OMNIPARSE_SKIP_MODEL_FETCH=1 cargo build --release
```

### Staged layout

After a successful release build, `<package>/bin/` contains:

| Path | Size (approx) | Notes |
|------|---------------|-------|
| `solx-omniparse-process-file[.exe]` | 25–40 MB | The Rust binary, linked statically against `rten` + `ocrs` (no FFI / no native DLLs) |
| `models/text-detection.rten` | ~3 MB | `ocrs` text-detection model, SHA-256 verified against upstream pin |
| `models/text-recognition.rten` | ~9 MB | `ocrs` text-recognition model, SHA-256 verified against upstream pin |

The two `.rten` files are SHA-256 pinned to the values in
`omniparse/src/ocr/ml.rs` and re-verified on every build. A mismatch deletes
the partial file and emits a `cargo:warning=` — the build still succeeds,
the binary still works (it will lazy-download on first OCR use).

The model URLs are the same as upstream's:

- `https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten`
- `https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten`

### Runtime auto-wiring of `OMNIPARSE_OCR_MODELS`

When the binary starts, it looks for `bin/models/text-detection.rten` and
`bin/models/text-recognition.rten` next to itself. If both are present it
sets `OMNIPARSE_OCR_MODELS` to that directory automatically — so first-run
OCR is instant with no network round-trip and no manual configuration. The
user's existing `OMNIPARSE_OCR_MODELS` value (if any) wins.

To override the staged path (shared cache, air-gapped install, etc.), set
`OMNIPARSE_OCR_MODELS` in the shell that launches `solx` before invoking the
action — the auto-wire will skip.

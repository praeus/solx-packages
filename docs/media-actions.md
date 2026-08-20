# Plan: Create `solx-media` package (media extraction actions)

> **Document note:** This file is the canonical plan + implementation log for the `solx-media` package. The plan body below was finalized before implementation; the appendices at the bottom track what shipped, followups, and design notes.
>
> **Superseded route:** `solx-server` moved to a REST surface on 2026-08-19; documents are now saved with `PUT /docs/{path}/{name}` (a bare `DocumentInput` as the body), not `POST /docs/save`. Mentions of `/docs/save` below are left as written because this is a dated log — see `solx-core/docs/http-api.md` for the current routes, and `persist.rs` in each package for the current code.

## Implementation status

- **v1 (core package)** — shipped 2026-08-14. `d:\Projects\solx-packages\solx-media\` exists with 26 files, 5 actions registered, `MediaDocument` JSON-Schema registered in `solx-types/seed.rs`. 18/18 unit tests passing (including 3 FU-1 return-shape tests). Release binary at `bin/solx-media.exe`.
- **FU-1: return-docs mode for solx-media** — shipped 2026-08-14, **removed 2026-08-17** in an action-audit pass. `--return` flag in `argv[2]` switched every extraction mode to caller-persists semantics via 4 actions (`solx-media-image-return`, `-audio-return`, `-video-return`, `-materialize-html-return`). Removed because the persisting actions already soft-fail to returning the raw document (with an empty `saved` list) on a `/docs/save` failure — the `-return` variants were a duplicate of what a user/model would actually invoke, not a distinct capability. `return_document()`/`return_mode` plumbing removed from `src/main.rs` along with the action registrations.
- **FU-2: write-docs mode for solx-omniparse** — shipped 2026-08-14. `--write` flag in `argv[1]` triggers `POST /docs/save` of the extracted text before printing. 1 new action registered (`solx-omniparse-process-file-write`). Implementation: `src/persist.rs` + `write_to_solx_server()` in main.rs + `#[tokio::main(flavor = "current_thread")]` (reqwest needs a Tokio reactor; `pollster::block_on` alone isn't enough).
- **FU-3: `rel_path` input** — not started; documented in the original plan's "Followups (post-v1)" section at the bottom of this file.

## TL;DR

Stand up `d:\Projects\solx-packages\solx-media\` as a sibling of `solx-omniparse`. A single `solx-media.exe` reads JSON from stdin, dispatches on a `mode` field to image / audio / video / materialize-html / install-whisper-model, downloads ffmpeg via `ffmpeg-sidecar` on first run, and (for the 4 extraction modes) calls Ollama + ffmpeg + whisper to produce a `MediaDocument` result which it persists to solx-server via `POST /docs/save`. Action registration uses the standard `install.solx` / `save type` / `save action` pattern; a new `MediaDocument` JSON-Schema type is added to `solx-types/seed.rs` as the canonical result shape.

**Key decisions** (from clarifying questions):
- One binary, mode-flag dispatch (not one binary per verb)
- Actions: `image` / `audio` / `video` / `materialize-html` / `install-whisper-model` (v1 skips `fetch-url`)
- Inputs: `source_path` only (no `rel_path` or `bytes_base64`)
- Config: env vars only (`OLLAMA_URL`, `MULTIMEDIA_MODEL`, `SUMMARIZER_MODEL`, `WHISPER_MODEL_PATH`, `WHISPER_MODELS_DIR`, `SOLX_SERVER_URL`, `SOLX_SERVER_TOKEN`, `SOL_LOG_DIR`, `SOLX_PACKAGES_DIR`)
- Whisper: `install-whisper-model` action with `name` param (default `tiny.en`); env-var override
- FFmpeg: `ffmpeg-sidecar` auto-download on first run
- Persist: action POSTs to solx-server `entity_save_document` (no return-documents-then-caller-persists)
- Result type: single flat `MediaDocument` JSON-Schema in `solx-types/seed.rs`
- Location: `d:\Projects\solx-packages\solx-media` (sibling of `solx-omniparse`, NOT in `solx-core`)

## Architecture

### Package layout

```
d:\Projects\solx-packages\solx-media/
├── .gitignore                  # .build-target/, bin/, target/, prompts/
├── Cargo.toml                  # crate solx-media, bin solx-media
├── Cargo.lock                  # cargo-generated
├── README.md                   # mirrors solx-omniparse/README.md structure
├── build.rs                    # nested-release-build + stage to bin/
├── install.solx                # save type MediaDocument, save type MediaInstallParams, save action x5
├── uninstall.solx              # delete action x5, delete type x2
├── package.json                # {name: "solx-media", version: "0.1.0", description}
├── src/
│   ├── main.rs                 # stdin JSON -> dispatch on mode -> result
│   ├── config.rs               # env-var config struct + validation
│   ├── ollama.rs               # generate / generate_with_image against {OLLAMA_URL}
│   ├── ffmpeg_setup.rs         # ffmpeg_sidecar::download::auto_download (OnceLock)
│   ├── whisper_models.rs       # WHISPER_MODEL_CATALOG + install/verify
│   ├── materialize.rs          # HTML rich-text materializer (data/URL/path -> EmbeddedArtifact)
│   ├── vision.rs               # run_image (image -> Ollama multimedia)
│   ├── audio.rs                # run_audio (audio -> whisper -> summarizer)
│   ├── video.rs                # run_video (video -> whisper + scene-frames + summarizer)
│   ├── prompt.rs               # bundled prompt loader (include_str! in src/prompts/)
│   ├── persist.rs              # POST {SOLX_SERVER_URL}/docs/save with bearer token
│   ├── log_mirror.rs           # stderr + $SOL_LOG_DIR/solx-media.log
│   ├── sha256_inline.rs        # hand-rolled FIPS 180-4 SHA-256 (shared with build.rs)
│   └── prompts/                # bundled via include_str!
│       ├── extraction-image-describe.prompt.txt
│       ├── extraction-audio-synthesize.prompt.txt
│       └── extraction-video-synthesize.prompt.txt
└── bin/                        # gitignored, staged by build.rs
    └── solx-media.exe
```

### Action surface (install.solx registers 5 actions + 2 types)

#### Type 1: `/packages/solx-media/MediaDocument`

One flat JSON-Schema type used as the `result_type_ref` for all 4 extraction modes. Discriminator via `kind`:

```json
{
  "description": "Result of a solx-media extraction. Persisted to solx-server via entity_save_document.",
  "schema": {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "type": "object",
    "properties": {
      "kind": {"enum": ["image-text", "audio-transcript", "video-transcript", "materialized-html"]},
      "document_name": {"type": "string"},
      "title": {"type": ["string", "null"]},
      "summary": {"type": ["string", "null"]},
      "author": {"type": ["string", "null"]},
      "contents": {"type": "object"},
      "artifacts": {"type": "array", "items": {"$ref": "#/$defs/EmbeddedArtifact"}},
      "transcript": {"type": "string"},
      "segments": {"type": "array", "items": {"$ref": "#/$defs/TimecodedSegment"}},
      "scene_captions": {"type": "array", "items": {"$ref": "#/$defs/TimecodedSegment"}},
      "description": {"type": "string"},
      "notes": {"type": "array", "items": {"type": "string"}}
    },
    "required": ["kind", "document_name", "contents"]
  }
}
```

(`#$defs/EmbeddedArtifact` and `#$defs/TimecodedSegment` defined inline in the same schema.)

#### Type 2: `/packages/solx-media/MediaInstallParams`

```json
{
  "description": "Params for solx-media install-whisper-model.",
  "schema": {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "type": "object",
    "properties": {
      "name": {"type": ["string", "null"], "description": "Whisper model name (e.g. tiny.en). Default: tiny.en"},
      "force": {"type": "boolean", "description": "Re-download even if file exists. Default: false"}
    }
  }
}
```

#### Action 1: `/packages/solx-media/solx-media-image`

`action_type: "command"`, `fn_name: ".\\solx-media.exe image"`, `caption: "Extract image to document"`, `category: "media"`, `description: "Vision-LLM pass: load image, ask Ollama multimedia model, return MediaDocument."`, `capabilities: ["media", "image", "vision", "extraction", "command"]`, `phrases: ["describe image", "extract image", "image to document", "vision extract"]`, `result_type_ref: /packages/solx-media/MediaDocument`, `action_config: {cwd: "D:/Projects/solx-packages/solx-media/bin", timeout_secs: 600}`.

The per-action `fn_name` passes the verb as a CLI arg. The binary infers mode from `argv[1]`. The `param_type_ref` only carries `{"source_path", "file_name"}`.

#### Action 2: `/packages/solx-media/solx-media-audio`

`fn_name: ".\\solx-media.exe audio"`. `phrases: ["transcribe audio", "extract audio", "audio to document"]`. `timeout_secs: 600`.

#### Action 3: `/packages/solx-media/solx-media-video`

`fn_name: ".\\solx-media.exe video"`. `phrases: ["transcribe video", "extract video", "video to document"]`. `timeout_secs: 1200`.

#### Action 4: `/packages/solx-media/solx-media-materialize-html`

`fn_name: ".\\solx-media.exe materialize-html"`. Accepts `source_path` (local HTML) + optional `source_url`. Internally calls `materialize::run_materialize_html`. `timeout_secs: 600`.

#### Action 5: `/packages/solx-media/solx-media-install-whisper-model`

`fn_name: ".\\solx-media.exe install-whisper-model"`, `param_type_ref: /packages/solx-media/MediaInstallParams`, `phrases: ["install whisper model", "download whisper model"]`, `timeout_secs: 300`.

### Binary input/output contract

- **Input:** JSON on stdin. `{"source_path": "C:/.../foo.png", "file_name": "foo.png", "source_url": "https://..."}`.
- **Output (extraction modes):** `{ "saved": [{ "path": "/...", "name": "..." }], "kind": "image-text", "document": { ... MediaDocument fields ... } }` on stdout. Exit 0 on success; non-zero on hard error. Persist failures are soft (warn to stderr, action still returns the document).
- **Output (install-whisper-model):** `{ "name": "tiny.en", "path": "C:/Users/.../ggml-tiny.en.bin", "size_bytes": 77691713, "sha256": "..." }` on stdout.
- **Env vars** (via `config.rs`):
  - `OLLAMA_URL` (default `http://localhost:11434`)
  - `MULTIMEDIA_MODEL` (default `llava`)
  - `SUMMARIZER_MODEL` (default `llama3.1`)
  - `WHISPER_MODEL_PATH` (optional override)
  - `WHISPER_MODELS_DIR` (default `{SOLX_PACKAGES_DIR}/solx-media/models`)
  - `SOLX_SERVER_URL` (required for persistence)
  - `SOLX_SERVER_TOKEN` or `SOLX_TOKEN` (required for persistence)
  - `SOL_LOG_DIR` (default `{SOLX_APPDATA_DIR}/solx-logs`)
  - `SOLX_PACKAGES_DIR` (default `{SOLX_APPDATA_DIR}/packages`; exported by solx-server from `solx-config.json`'s packages-directory field, the same way other root directories are). Used as the root for `{SOLX_PACKAGES_DIR}/solx-media/config.json`. **Recommended** also for `WHISPER_MODELS_DIR` default (`{SOLX_PACKAGES_DIR}/solx-media/models`) so all solx-media on-disk state lives under one tree; still overridable per-env.
  - `SOLX_MEDIA_SKIP_AUTOBUILD` / `SOLX_MEDIA_SKIP_FFMPEG_FETCH`

### FFmpeg acquisition

Mirror `sol-manager/src/manager.rs:55-71`:

```rust
// src/ffmpeg_setup.rs
use std::sync::OnceLock;
static FFMPEG_SETUP: OnceLock<Result<(), String>> = OnceLock::new();

pub fn ensure_ffmpeg_available() -> Result<(), String> {
    FFMPEG_SETUP
        .get_or_init(|| ffmpeg_sidecar::download::auto_download()
            .map_err(|e| format!("ffmpeg auto-download failed: {e}")))
        .clone()
}
```

Called from `main()` before any ffmpeg work. No `install.solx` step required.

### Whisper model installation

`src/whisper_models.rs` mirrors `sol-manager/src/whisper.rs:1-100`:

- Hard-coded `WHISPER_MODEL_CATALOG: &[WhisperModelSpec]` with ~10-15 popular models (tiny/tiny.en/base/base.en/small/small.en/medium/medium.en/large-v3 for English, plus tdrz variants). Each: `{name, url, sha256, size_bytes}`.
- `pub fn install(name: Option<String>, force: bool) -> Result<InstalledModel, String>`:
  1. Resolve `name` (default `tiny.en`).
  2. Compute `dest = WHISPER_MODELS_DIR/ggml-{name}.bin`.
  3. If `dest` exists and `!force`: SHA-256 it, verify against pinned hash, return early.
  4. Otherwise: download via `reqwest::Client`.
  5. SHA-256 verify -> atomic rename to `dest`.
  6. If `WHISPER_MODEL_PATH` is unset, set it to the new `dest` and persist the active model name into `{SOLX_PACKAGES_DIR}/solx-media/config.json` (creates the file/dir if missing). Also persists any other package-level settings that have been changed at runtime (see "Media config file" below).

- `pub fn active_model_path() -> Option<PathBuf>`: env var first, then config file's `active_whisper_model`, then scan `WHISPER_MODELS_DIR` for any `ggml-*.bin` (lex-smallest first), else None.

### Media config file

Single JSON config at `{SOLX_PACKAGES_DIR}/solx-media/config.json`. Created lazily on first write; never required to exist (env vars cover the common case).

```json
{
  "active_whisper_model": "tiny.en",
  "multimedia_model": "llava",
  "summarizer_model": "llama3.1"
}
```

Field semantics:
- `active_whisper_model` (string) — name passed to a subsequent `install-whisper-model`; `WHISPER_MODEL_PATH` resolution falls back to this when env unset.
- `multimedia_model` (string) — overrides `MULTIMEDIA_MODEL` env var if set in the file (env still wins).
- `summarizer_model` (string) — overrides `SUMMARIZER_MODEL` env var if set in the file (env still wins).

Read/write access is best-effort and never fatal: a missing/unreadable/unparseable file is treated as "no overrides", and write failures are logged to stderr but don't abort the action.

Future fields (v1.1+) can include `ollama_url`, per-kind timeouts, prompt overrides, etc., without a schema migration — unknown fields are ignored on read.

- The SHA-256 in `install()` reuses the hand-rolled FIPS 180-4 implementation from `solx-omniparse/build.rs` -- extract it into a `sha256_inline` module that both `build.rs` and `whisper_models.rs` import.

### Persist to solx-server

`src/persist.rs` exposes:

```rust
pub async fn save_document(
    server_url: &str,
    token: &str,
    document: &MediaDocument,
) -> Result<(String, String), String>  // (saved_path, saved_name)
```

- POSTs `{server_url}/docs/save` with `Authorization: Bearer {token}` and JSON body `{"path": "/media", "name": ..., "document": {...}}`.
- Returns `(path, name)` on 200; surfaces the API error on non-2xx.

**Naming convention:** `/media/{kind}/{document_name}`. E.g. `/media/image-text/cat-photo.md`.

### Prompt templates

Three new files in `src/prompts/`. Bundled via `include_str!("prompts/extraction-image-describe.prompt.txt")`. Placeholders use the same `{{key}}` / `{{ key }}` syntax as the old system.

`src/prompt.rs` provides:

```rust
pub fn load(name: &str) -> Result<String, String> {
    match name {
        "extraction-image-describe.prompt.txt" => Ok(include_str!("prompts/extraction-image-describe.prompt.txt").to_string()),
        "extraction-audio-synthesize.prompt.txt" => Ok(include_str!("prompts/extraction-audio-synthesize.prompt.txt").to_string()),
        "extraction-video-synthesize.prompt.txt" => Ok(include_str!("prompts/extraction-video-synthesize.prompt.txt").to_string()),
        _ => Err(format!("unknown prompt: {name}")),
    }
}

pub fn render(template: &str, vars: &[(&str, &str)]) -> String { /* {{key}} replace */ }
```

## Steps

### Phase 1: Foundations

1. **Add `MediaDocument` JSON-Schema type to solx-types**
   - File: `d:\Projects\solx-core\solx-types\src\seed.rs`
   - Add a new entry to `builtin_types()`: `(name: "MediaDocument", schema: MediaDocumentSchema)`.
   - Reuses the existing pattern (`HtmlDocument`, `RichTextDoc` are already seeded there).
   - Verify: `cargo test -p solx-types` passes; `solx-types::LocalTypeManager::get("/builtin/types/MediaDocument")` returns the schema.

2. **Create package skeleton** *(parallel with step 1)*
   - Create `d:\Projects\solx-packages\solx-media\` with: `.gitignore`, `package.json`, `Cargo.toml` (matching omniparse's `[workspace]` empty, deps: `base64`, `ffmpeg-sidecar = { workspace = true }`, `reqwest = { version = "0.12", features = ["json", "rustls-tls"] }`, `serde`, `serde_json`, `url`, `uuid`, `tokio = { features = ["full"] }`, `anyhow`).

3. **Copy SHA-256 helpers from omniparse's build.rs** *(depends on step 2)*
   - New file: `solx-media/build.rs` -- initial version contains SHA-256 helpers and nested-release-build scaffolding.
   - Verify: `cargo build -p solx-media` triggers the nested build; `bin/solx-media.exe` exists.

### Phase 2: Core media pipeline

4. **`config.rs` -- env-var config** *(depends on step 2)*
   - `struct MediaConfig { ollama_url, multimedia_model, summarizer_model, whisper_model_path, whisper_models_dir, server_url, server_token, log_dir }`.
   - `MediaConfig::from_env() -> Result<Self, String>`.
   - Tests: `#[cfg(test)]` covers the env-override logic.

5. **`ollama.rs` -- Ollama HTTP client** *(parallel with step 4)*
   - `async fn generate(model, prompt, base_url) -> Result<String, String>` -- `POST {base_url}/api/generate` with `{"model": ..., "prompt": ..., "stream": false}`, parse `response` field.
   - `async fn generate_with_image(model, prompt, image_b64, base_url) -> Result<String, String>` -- same with `images: [b64]`.
   - Mirrors `sol-manager/src/providers/ollama.rs`.

6. **`materialize.rs` -- HTML rich-text media materializer** *(parallel with steps 4, 5)*
   - Port `materialize_document_rich_text_media`, `extract_embedded_artifacts_from_rich_text`, `load_media_asset`, `parse_data_url`, `fetch_remote_media`, `derive_media_artifact_name`, the four `extension_from_content_type` / `content_type_from_*` helpers verbatim.
   - Function signature changes: instead of `Vec<ExtractedDocumentPayload>`, return `Vec<MediaDocument>`.

7. **`vision.rs` -- image extraction** *(depends on steps 4, 5)*
   - `pub async fn run_image(bytes, file_name, cfg) -> Result<MediaDocument, String>`.
   - Build prompt via `prompt::load("extraction-image-describe.prompt.txt")` + `prompt::render(...)` with `{{file_name}}` and `{{image_metadata}}` (just `format!("filename: {file_name}, file_size: {} bytes", bytes.len())` -- no `image` crate dep in v1).
   - `ollama::generate_with_image(...)`.
   - Parse JSON response as `ImageDescriptionResponse` shape.
   - Return `MediaDocument { kind: "image-text", ... }`.

8. **`audio.rs` -- audio extraction** *(depends on steps 4, 5, 6)*
   - `pub async fn run_audio(bytes, file_name, cfg) -> Result<MediaDocument, String>`.
   - `write_bytes_to_temp(bytes, ext)` + `TempFileGuard`.
   - `try_ffmpeg_whisper_transcribe(&temp_path, &cfg)`.
   - On failure, fall back to `ffmpeg_extract_audio_wav` -> re-transcribe.
   - If `cfg.summarizer_model` is set: build audio-synthesize prompt, `ollama::generate(...)`, parse `MediaSynthesisResponse`.
   - Return `MediaDocument { kind: "audio-transcript", transcript, segments, ... }`.

9. **`video.rs` -- video extraction** *(depends on step 8)*
   - Same as audio + `ffmpeg_extract_frames(&temp_path, 20)` + per-frame vision captions via `ollama::generate_with_image` -> `TimecodedSegment`.
   - Use the video-synthesize prompt that includes scene descriptions.

10. **`ffmpeg_setup.rs` + `whisper_models.rs`** *(depends on step 3)*
    - `ffmpeg_setup::ensure_ffmpeg_available()` -- `ffmpeg_sidecar::download::auto_download()` with `OnceLock` caching.
    - `whisper_models::install(name, force)` -- port from sol-manager/whisper.rs: `WHISPER_MODEL_CATALOG`, `install`, `active_model_path`, `WhisperModelSpec` struct.
    - Reuse SHA-256 helpers from `build.rs` via `sha256_inline.rs` module.

### Phase 3: Wiring

11. **`persist.rs` -- save to solx-server** *(depends on step 1)*
    - `pub async fn save_document(server_url, token, doc) -> Result<(String, String), String>`.
    - POST to `{server_url}/docs/save` with `Authorization: Bearer {token}` and a `Document` body shaped for `entity_save_document`.
    - Returns `(path, name)` on 200; surfaces the API error on non-2xx.

12. **`main.rs` -- dispatch** *(depends on steps 4-11)*
    - Read JSON from stdin.
    - Set up `log_mirror` to `$SOL_LOG_DIR/solx-media.log`.
    - `let cfg = MediaConfig::from_env()?;`.
    - Match on `argv[1]`:
      - `"image"`: `vision::run_image` -> `persist::save_document`.
      - `"audio"`: `audio::run_audio` -> persist.
      - `"video"`: `video::run_video` -> persist.
      - `"materialize-html"`: `materialize::run_materialize_html` -> persist.
      - `"install-whisper-model"`: `whisper_models::install` -> print JSON.
    - Print summary JSON `{ "saved": [...], "kind": "...", "document": {...} }` to stdout.

13. **`install.solx` / `uninstall.solx`** *(depends on steps 1, 12)*
    - **install.solx** (5 actions + 2 types): `save type MediaDocument`, `save type MediaInstallParams`, `save action solx-media-{image,audio,video,materialize-html,install-whisper-model}`.
    - **uninstall.solx**: `delete action` for all 5 in reverse, then `delete type` for both.

### Phase 4: Docs + verification

14. **README.md** *(parallel with step 13)*
    - Mirror `solx-omniparse/README.md` structure: install, runtime model (env vars), ffmpeg auto-wiring, whisper model install, action surface, troubleshooting.
    - Note the `action_config.cwd` must be edited to absolute path before install.

15. **Smoke test (manual)** *(depends on all of the above)*
    - `cargo build -p solx-media --release` -> produces `bin/solx-media.exe`.
    - Edit `install.solx` `action_config.cwd` to absolute path.
    - `solx install-package D:\Projects\solx-packages\solx-media`.
    - `solx exec /packages/solx-media/solx-media-install-whisper-model --json '{}'` -> downloads `tiny.en`.
    - Run each extraction action against sample fixtures.
    - `solx list docs /media/image-text` shows the persisted document.

## Relevant files

### Created
- `d:\Projects\solx-packages\solx-media\{.gitignore, package.json, Cargo.toml, build.rs, install.solx, uninstall.solx, README.md}`
- `d:\Projects\solx-packages\solx-media\src\{main, config, ollama, ffmpeg_setup, whisper_models, materialize, vision, audio, video, prompt, persist, log_mirror, sha256_inline}.rs`
- `d:\Projects\solx-packages\solx-media\src\prompts\{extraction-image-describe, extraction-audio-synthesize, extraction-video-synthesize}.prompt.txt`

### Modified
- `d:\Projects\solx-core\solx-types\src\seed.rs` -- add `MediaDocument` schema entry to `builtin_types()`

### Reused (read-only references)
- `d:\Projects\sol\sol-manager\src\extraction\media.rs` -- source of truth for the refactor
- `d:\Projects\sol\sol-manager\src\whisper.rs` -- `WHISPER_MODEL_CATALOG` and `WhisperModelSpec`
- `d:\Projects\sol\sol-manager\src\llm_service.rs` + `src\providers\ollama.rs` -- Ollama HTTP request shape
- `d:\Projects\sol\sol-manager\src\prompt_store.rs` -- `{{key}}` substitution logic
- `d:\Projects\sol\sol-manager\src\extraction\utils.rs` -- `normalize_identifier` and `parse_llm_json`
- `d:\Projects\solx-packages\solx-omniparse\build.rs` -- SHA-256 helpers and nested-build scaffolding
- `d:\Projects\solx-packages\solx-omniparse\src\main.rs` -- stdin-JSON, stderr-mirror, log-file pattern
- `d:\Projects\solx-core\solx-types\src\seed.rs` -- pattern for adding `MediaDocument` (mirror `HtmlDocument` entry)
- `d:\Projects\solx-core\solx-actions\src\internal\file.rs` -- `file_put` parameter shape (for `MaterializedMediaAsset` analog)

## Verification

1. `cargo build -p solx-media --release` -- produces `bin/solx-media.exe`; build.rs exits without warnings.
2. `bin/solx-media.exe` with no args prints usage to stderr, exit 2.
3. `bin/solx-media.exe image` with `{"source_path": "C:/test.png"}` on stdin -- after first run, ffmpeg-sidecar downloads ffmpeg to `%LOCALAPPDATA%/ffmpeg-sidecar/`. Second run is instant.
4. With `SOLX_SERVER_URL=http://127.0.0.1:8766 SOLX_SERVER_TOKEN=<from-server-startup>`: the image action POSTs to `/docs/save` and the document is queryable via `solx list docs /media/image-text`.
5. `solx exec /packages/solx-media/solx-media-install-whisper-model --json '{}'` -- downloads `ggml-tiny.en.bin` (~75 MB) into `{SOLX_PACKAGES_DIR}/solx-media/models/` (default `%APPDATA%/praeus/solx/packages/solx-media/models`). Re-running is a no-op (cached + SHA-verified); `{SOLX_PACKAGES_DIR}/solx-media/config.json` records `active_whisper_model: "tiny.en"` on first install.
6. `solx exec /packages/solx-media/solx-media-audio --json '{"source_path": "C:/test.mp3", "file_name": "test.mp3"}'` -- produces an `AudioTranscriptDocument` with transcript + segments.
7. `solx exec /packages/solx-media/solx-media-video --json '{"source_path": "C:/test.mp4", "file_name": "test.mp4"}'` -- produces a `VideoTranscriptDocument` with transcript + segments + scene_captions.
8. `solx exec /packages/solx-media/solx-media-materialize-html --json '{"source_path": "C:/page.html", "source_url": "https://example.com/page"}'` -- embeds images referenced in the page.
9. `solx uninstall-package solx-media` then `solx install-package D:\Projects\solx-packages\solx-media` -- clean install/uninstall cycle, no orphan rows.
10. `cargo test -p solx-types` -- passes (the new `MediaDocument` schema is registered).
11. `solx list types /builtin/types/MediaDocument` -- returns the schema.

## Decisions & assumptions

- **One flat `MediaDocument` type** (not the full hierarchy of `ExtractedDocumentPayload` / `RichTextDoc` / `HtmlDocument`). The flat shape is sufficient for the solx-media result; richer shapes can be added later if a consumer needs them.
- **Action persistence to solx-server is the action's responsibility** (not the caller's). This deviates from the omniparse pattern (which just returns bytes on stdout). The user explicitly chose this -- the action does `POST /docs/save` itself.
- **No `DbManager` trait dependency in solx-media.** The old code used `db.get_model(...)` / `db.get_model_by_role(...)` to look up model config. The new package uses env vars + the install action's `name` param. A small `ConfigService` is unnecessary for v1.
- **No `EventHooksConfig` in v1.** The `on_prompt_load` hook override is not replicated; bundled prompts are the only source.
- **No `WhisperModel` install UI beyond `install-whisper-model`.** The catalog is hard-coded in the binary; `list-models` was not selected by the user (skipped to keep the action surface minimal). Models are documented in README.
- **No `image` crate dependency.** `build_image_metadata_string` (the `image::load_from_memory` for width/height detection) is dropped in v1. The image prompt's `{{image_metadata}}` is just filename + size. Easy to add later if needed.
- **`fetch_url_html` is not in v1** (user said "or consider alternatives"; the action is `materialize-html` which already accepts a `source_path` from any prior fetch).
- **`action_config.cwd` is hardcoded in install.solx** (matches omniparse). User must edit the absolute path before installing, OR re-`save action` afterward with the correct path.

## Further considerations

1. **Should solx-media also accept a `rel_path` input** (in addition to `source_path`)? This would let other actions chain into it via the file store without needing the file on disk. *Recommendation: defer to v2 (see Followups below).*
2. **Where should the `MediaDocument` schema actually live** -- `solx-types/src/seed.rs` (proposed) vs. a new `solx-extract-types` shared crate? *Decision: in `solx-types/src/seed.rs` for now* (single source of truth, no new crate to maintain). Can be re-homed later if multiple packages need the type.
3. **Should the whisper-model install write to a config file** (so `WHISPER_MODEL_PATH` is auto-set on subsequent runs) or only to disk? *Decision: yes, persist to `{SOLX_PACKAGES_DIR}/solx-media/config.json`* (single config file holds `active_whisper_model` + `multimedia_model` + `summarizer_model`; matches the old sol-manager pattern in `whisper_model_path()` / `set_whisper_model_path()` and is overridable through the same `solx-config.json` packages-directory setting other directories use).

## Followups (post-v1)

These are intentionally **out of scope for v1** but will be needed soon after; documenting the shape now so they fit cleanly into the v1 architecture.

### FU-1: `solx-media` return-documents-then-caller-persists mode

Add a second action per kind (`solx-media-image-return`, `solx-media-audio-return`, `solx-media-video-return`, `solx-media-materialize-html-return`) whose `fn_name` passes a `--return` flag instead of persisting. Output mirrors v1's stdout JSON but `saved: []` and `document` is the full `MediaDocument` the caller can post to `entity_save_document` itself.

Implementation:
- Add a `--return` flag the binary reads alongside `argv[1]` (e.g. `argv[2] == "--return"` or `--mode=return`).
- In `main.rs`, branch on the flag: persist path is the v1 default; return path skips `persist::save_document` and prints `{"document": {...}, "kind": "..."}` to stdout.
- Reuses every existing function unchanged; the action surface doubles but the source-of-truth modules don't grow.
- `install.solx` adds 4 `save action` lines.

### FU-2: `solx-omniparse` write-document mode

Mirror the same idea for the omniparse package: add a `solx-omniparse-process-file-write` action that calls the existing binary with a `--write` flag, parses the `OmniparseResult` on stdout, and POSTs to `{SOLX_SERVER_URL}/docs/save` itself before returning the saved path/name to the caller. Uses the same persist.rs pattern from solx-media (a `POST /docs/save` with bearer token).

Goal: both `solx-omniparse` and `solx-media` can be invoked either way (action-persists vs. caller-persists) so workflows can pick the most ergonomic shape per call site.

### FU-3: `rel_path` input support

Extend `solx-media`'s action param shape to accept either:
- `source_path` (local file path; v1 behavior), or
- `rel_path` (a `files/...` pointer into the solx-server file store — caller does `file_put` first).

Resolution order: if `rel_path` is present, read bytes from the file store via `GET /files/get` (or a local fast-path if the binary is on the same machine as the server); otherwise read `source_path` from disk. Same `MediaDocument` result and persist path.

Implementation:
- `config.rs` adds `server_url` + `server_token` (already required for persist; now also for rel_path fetch).
- `main.rs` adds a `load_bytes(params, cfg) -> Result<Vec<u8>, String>` helper that picks the right source.
- No action-schema change required at the call site — `param_type_ref` adds `rel_path: { type: ["string", "null"] }` to the existing shape; the v1 `source_path` callers continue to work unchanged.

These three followups are mutually independent; FU-1 and FU-2 can ship in parallel since they touch different packages. FU-3 is a v2 input-shape change that doesn't conflict with FU-1/FU-2.

## Followup 1 — implementation plan: `solx-media` return-docs mode (FU-1)

### Goal

Add a second action per kind whose `fn_name` ends in `-return`. These actions skip the internal `POST /docs/save` and instead return the `MediaDocument` for the caller to persist (via `entity_save_document` or any other path). Same source code path, just a different dispatch branch.

### New actions

| Action | Verb (argv[1]) | Behavior |
|---|---|---|
| `solx-media-image-return` | `image --return` | Run vision, return `{kind, document}` |
| `solx-media-audio-return` | `audio --return` | Run whisper + synthesis, return `{kind, document}` |
| `solx-media-video-return` | `video --return` | Run whisper + frames + synthesis, return `{kind, document}` |
| `solx-media-materialize-html-return` | `materialize-html --return` | Walk HTML, return `{kind, document}` |

All four reuse the v1 source functions (`vision::run_image`, etc.) and the `MediaDocument` schema.

### Binary contract changes

- `argv[2]` may equal `--return`. When set, the action returns the document instead of persisting.
- Output on `--return`:

    ```json
    {
      "kind": "image-text",
      "document_name": "cat-photo",
      "document": { ...MediaDocument fields... }
    }
    ```

    (Mirrors the v1 `document` sub-object plus `kind` for caller convenience. The full `document` IS the MediaDocument — caller passes it straight to `entity_save_document`.)

- Exit code: 0 on success, non-zero on hard error (whisper failure, ffmpeg failure, malformed input). No "soft" exit because there's no persist to fail-soft on.

### Code changes

`src/main.rs`:

```rust
async fn mode_image(client, cfg, input) -> Result<Value, String> {
    // ... same extraction ...
    let doc = vision::run_image(client, &bytes, file_name.as_deref(), cfg).await?;
    if return_mode {
        // Skip persist; return doc as-is.
        Ok(json!({ "kind": doc["kind"], "document_name": doc["document_name"], "document": doc }))
    } else {
        save_and_summarize(client, cfg, doc).await
    }
}
```

Apply the same branch to `mode_audio`, `mode_video`, `mode_materialize_html`. The persist call is the only thing that changes; the extraction logic is untouched.

`return_mode` is computed once in `main()` from `argv[2] == "--return"`.

### install.solx additions (4 lines)

```solx
save action /packages/solx-media/solx-media-image-return --json '{"action_type":"command","fn_name":".\\solx-media.exe image --return",...same as v1 image except timeout_secs unchanged}';
save action /packages/solx-media/solx-media-audio-return --json '{"action_type":"command","fn_name":".\\solx-media.exe audio --return",...}';
save action /packages/solx-media/solx-media-video-return --json '{"action_type":"command","fn_name":".\\solx-media.exe video --return",...}';
save action /packages/solx-media/solx-media-materialize-html-return --json '{"action_type":"command","fn_name":".\\solx-media.exe materialize-html --return",...}';
```

Each reuses `result_type_ref: /packages/solx-media/MediaDocument` (no new type needed).

### uninstall.solx additions (4 lines)

Mirror in reverse: `delete action` for the 4 new actions.

### Why `--return` as `argv[2]` instead of a separate verb

- Single dispatch table in `main.rs` — `argv[1]` picks the extraction; `argv[2]` toggles persist.
- Per-action `fn_name` reads naturally: `".\\solx-media.exe image --return"`.
- No new source modules; only `main.rs` changes by ~30 lines.

### Caller patterns after FU-1

Workflow author who wants the document in a script without server-side persist:

```solx
exec /packages/solx-media/solx-media-image-return --json '{"source_path":"C:/x.png"}' as $doc;
exec /builtin/document/entity_save_document --json '{path:"/media", name:$doc.document_name, document:$doc.document}';
```

## Followup 2 — implementation plan: `solx-omniparse` write-docs mode (FU-2)

### Goal

Add a `solx-omniparse-process-file-write` action that does what `solx-omniparse-process-file` does, **plus** POSTs the resulting `OmniparseResult` to solx-server's `POST /docs/save` itself before returning the saved path/name to the caller.

### Difference from v1

- v1 (`solx-omniparse-process-file`): binary prints `OmniparseResult` JSON to stdout. The caller (script or other action) does `entity_save_document` if they want it persisted.
- FU-2 (`solx-omniparse-process-file-write`): binary extracts, then in-process POSTs the result to `/docs/save`, then prints `{saved: [{path, name}], result: {...OmniparseResult...}}` to stdout.

### How

The current omniparse binary prints raw bytes on stdout; the FU-2 flag needs it to optionally POST first. Two options:

**(A) Add a `--write` flag** — same shape as FU-1. The binary reads the flag from `argv[1]` (since omniparse currently takes no CLI arg). Internally: after `print_json(result)`, if `--write` is set, do the POST and then print a second JSON line with `{"saved": [...]}`.

**(B) Add a separate `solx-omniparse-write` Rust binary** — cleaner separation but doubles the build artifact count.

**Recommend (A)** — same pattern as FU-1, smallest delta. The omniparse binary's `main.rs` adds a `--write` branch and a persist call.

### Code changes

`d:\Projects\solx-packages\solx-omniparse\src\main.rs`:

1. Add a `media_config.rs` module (or reuse solx-media's persist.rs via vendoring — see note below).
2. At the end of `main()`, before `print_json`, branch on `std::env::args().any(|a| a == "--write")`:
   - If set: read `SOLX_SERVER_URL` + `SOLX_SERVER_TOKEN` env vars, build a Document payload wrapping the bytes + extracted text, POST to `/docs/save`.
   - Print `{"saved": [{path, name}], "result": {...OmniparseResult fields...}}`.
   - If set but env vars missing: print warning to stderr, fall through to v1 behavior (return raw OmniparseResult). Soft-fail.

### Document payload for omniparse

Need a small `OmniparseDocument` type in `solx-types/seed.rs` — mirrors `MediaDocument` but for `OmniparseResult`. Discriminator `kind: "preprocessed-text"`, `contents.text` is the extracted text. Or simpler: re-use `MediaDocument` with `kind: "preprocessed-text"` and `contents: {"text": "..."}`. Decide during implementation; the JSON-Schema is the same shape.

### install.solx additions (1 line)

```solx
save action /packages/solx-omniparse/solx-omniparse-process-file-write --json '{"action_type":"command","fn_name":".\\solx-omniparse-process-file.exe --write","caption":"solx-omniparse process-file (write)","category":"extraction","description":"...","capabilities":["extraction","preprocess","ocr","write"],"phrases":["...","convert pdf to text and save","..."],"param_type_ref":"/packages/solx-omniparse/OmniparseInput","result_type_ref":"/packages/solx-omniparse/OmniparseResult","action_config":{"cwd":"D:/Projects/solx-packages/solx-omniparse/bin","timeout_secs":600}}';
```

Reuses the existing `OmniparseInput` / `OmniparseResult` types — no new types needed.

### Reusing solx-media's `persist.rs`

The two implementations want the same `POST /docs/save` shape. Two options:

**(A) Copy** `persist.rs` from solx-media into solx-omniparse, with minor adjustments (Document shape).

**(B) Extract** into a shared crate, e.g. `solx-persist` under `solx-core/`. Both packages depend on it.

**Recommend (A)** for v1 of FU-2 — keep the omniparse crate standalone (matches its current `[workspace]` empty design). If a third caller emerges, refactor to (B). Note this in a followup comment.

### Cargo.toml additions for omniparse

```toml
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }
```

(Already needed for `--write` mode.) Other deps: `serde`, `serde_json` already present.

### Why FU-2 lives in solx-omniparse, not solx-media

The omniparse binary's value comes from the OCR + format-detection + multi-tier PDF pipeline. The FU-2 action just adds a write step at the end. Bundling it into solx-media would mean solx-media has to call out to omniparse (subprocess or library), which is worse than letting omniparse own its own write mode.

### Caller patterns after FU-2

```solx
# The action persists itself (FU-2):
exec /packages/solx-omniparse/solx-omniparse-process-file-write --json '{...}' as $saved;
# $saved.saved[0].path is the persisted path; $saved.result has the raw bytes.
```

**Update (action-audit pass):** the v1 `solx-omniparse-process-file` action
(caller-persists-manually) was removed from `install.solx` — a duplicate of
what a user/model would actually invoke, since `-write` is the one that
creates a document directly and already soft-falls-back to a raw
(unsaved) result if the server is unreachable. The binary's raw-extraction
code path is unchanged internally; only the standalone registration is gone.

## Implementation order

1. **FU-1** first (smaller scope; only `src/main.rs` + `install.solx` + `uninstall.solx` in solx-media).
2. **FU-2** second (cross-package; needs `persist.rs` copy or shared module + `OmniparseDocument` schema registration).
3. **FU-3** last (largest; requires file-store API + new param shape).


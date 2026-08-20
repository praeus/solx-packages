# solx-media

Media extraction actions for solx-core: image (vision), audio (whisper), and video (whisper + scene captions), with persistence back to solx-server via `PUT /docs/{path}/{name}`.

Mirrors the structure of [solx-omniparse](../solx-omniparse): one binary, `install.solx` / `uninstall.solx`, command-type actions.

## Actions

| Action | Kind | Description |
|---|---|---|
| `solx-media-image` | `command` | Vision-LLM pass on an image → `MediaDocument` (kind=`image-text`) |
| `solx-media-audio` | `command` | Whisper-transcribe + summarize → `MediaDocument` (kind=`audio-transcript`) |
| `solx-media-video` | `command` | Whisper + per-frame captions + summarize → `MediaDocument` (kind=`video-transcript`) |
| `solx-media-materialize-html` | `command` | Fetch embedded images from an HTML rich-text payload → `MediaDocument` (kind=`materialized-html`) |
| `solx-media-install-whisper-model` | `command` | Download + SHA-verify a whisper.cpp ggml model from huggingface |

Each PUTs its result to `{SOLX_SERVER_URL}/docs/media/{kind}/{document_name}` itself and prints a
summary on stdout — this is what a user/model would actually invoke, so a
separate caller-persists-manually variant isn't registered as an action.
Saved documents land at `/media/{kind}/{document_name}`. If the save
fails (e.g. solx-server unreachable), the call soft-fails: it still returns
the full `document` on stdout with an empty `saved` list, so nothing is
lost — the caller can persist it via `entity_save_document` themselves.

## Install

1. Build (mirrors solx-omniparse):

    ```bash
    cargo build --release -p solx-media
    ```

    The build script auto-stages the binary at `<package>/bin/solx-media.exe`.

2. Edit `install.solx` — every `action_config.cwd` is hardcoded to `D:/Projects/solx-packages/solx-media/bin`. Update to your absolute path before installing.

3. Install:

    ```bash
    solx install-package D:/Projects/solx-packages/solx-media
    ```

4. Install a whisper model (one-time):

    ```bash
    solx exec /packages/solx-media/solx-media-install-whisper-model --json '{}'
    # or pick a model:
    solx exec /packages/solx-media/solx-media-install-whisper-model --json '{"name":"base.en"}'
    ```

## Configuration (env vars)

| Var | Default | Notes |
|---|---|---|
| `OLLAMA_URL` | `http://localhost:11434` | Ollama base URL (`POST /api/generate`) |
| `MULTIMEDIA_MODEL` | `llava` | Vision-capable Ollama model for image + scene captions |
| `SUMMARIZER_MODEL` | `llama3.1` | Ollama model for audio/video synthesis |
| `WHISPER_MODEL_PATH` | (unset) | Direct path to a ggml model (overrides auto-resolution) |
| `WHISPER_MODELS_DIR` | `{SOLX_PACKAGES_DIR}/solx-media/models` | Where `install-whisper-model` writes model files |
| `SOLX_SERVER_URL` | (required) | e.g. `http://127.0.0.1:8766` |
| `SOLX_SERVER_TOKEN` / `SOLX_TOKEN` | (required) | Bearer token from `solx-server` startup |
| `SOL_LOG_DIR` | (unset) | If set, log lines are mirrored to `{SOL_LOG_DIR}/solx-media.log` |
| `SOLX_PACKAGES_DIR` | `{SOLX_APPDATA_DIR}/packages` | Root for `{SOLX_PACKAGES_DIR}/solx-media/config.json` |
| `SOLX_MEDIA_SKIP_AUTOBUILD` | (unset) | Skip the build script's nested cargo build |

## Media config file

`{SOLX_PACKAGES_DIR}/solx-media/config.json` holds runtime-persisted settings:

```json
{
  "active_whisper_model": "tiny.en",
  "multimedia_model": "llava",
  "summarizer_model": "llama3.1"
}
```

The file is created lazily by `install-whisper-model`; missing/unreadable files are treated as "no overrides". Env vars always win over the file.

## Usage

```bash
# Image
solx exec /packages/solx-media/solx-media-image --json '{"source_path":"C:/photos/cat.png","file_name":"cat.png"}'

# Audio
solx exec /packages/solx-media/solx-media-audio --json '{"source_path":"C:/audio/lecture.mp3","file_name":"lecture.mp3"}'

# Video
solx exec /packages/solx-media/solx-media-video --json '{"source_path":"C:/videos/keynote.mp4","file_name":"keynote.mp4"}'

# Materialize HTML (input must contain a `rich_text` field — typically from omniparse output)
solx exec /packages/solx-media/solx-media-materialize-html --json '{"source_path":"C:/page.html","source_url":"https://example.com","rich_text":{...}}'
```

Each invocation prints a JSON summary like:

```json
{
  "saved": [{"path": "/media/image-text", "name": "cat-photo"}],
  "kind": "image-text",
  "document": { ...MediaDocument fields... }
}
```

List persisted documents:

```bash
solx list docs /media/image-text
```

## FFmpeg

Downloaded on first invocation via `ffmpeg-sidecar` (cached at `%LOCALAPPDATA%/ffmpeg-sidecar/` on Windows, `~/.cache/ffmpeg-sidecar/` on POSIX). Subsequent runs reuse the cached binary.

## Whisper models

The default is `tiny.en` (~78 MB). Other models available:

`tiny`, `tiny-q5_1`, `tiny.en-q5_1`, `tiny-q8_0`, `base`, `base.en`, `base-q5_1`, `base.en-q5_1`, `small`, `small.en`, `small.en-tdrz`, `medium`, `medium.en`, `large-v3`, `large-v3-turbo`.

The SHA-256 of `tiny` and `tiny.en` are pinned in `src/whisper_models.rs`; the rest are placeholder hashes (install proceeds but skips verification until you pin them).

## Smoke test

```bash
# 1. Build
cargo build --release -p solx-media

# 2. Install
solx install-package D:/Projects/solx-packages/solx-media

# 3. Install whisper model
solx exec /packages/solx-media/solx-media-install-whisper-model --json '{}'

# 4. Extract an image
solx exec /packages/solx-media/solx-media-image --json '{"source_path":"C:/test.png","file_name":"test.png"}'

# 5. List persisted docs
solx list docs /media/image-text
```

## Followups

- **FU-1**: return-docs mode (`*-return` actions) — shipped, then removed in
  an action-audit pass: each persisting action already soft-fails to
  returning the raw document on a save failure, so the separate
  caller-persists-manually actions were a duplicate of what a user/model
  would invoke, not a distinct capability.
- **FU-2**: ✅ shipped — `solx-omniparse-process-file-write` action.
- **FU-3**: not started — `rel_path` input support.

See `../docs/media-actions.md` for the full plan and status.

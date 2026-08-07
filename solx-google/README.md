# solx-google

Direct integration with Google Docs, Drive, Gmail, Google Tasks, and Google
Calendar over their REST APIs for solx-core. Uses native Webhook actions
that call `https://*.googleapis.com/...` directly, with OAuth credentials
encrypted and persisted to the scoped secret store.

> **Not covered:** Google Keep. The Keep API is Google Workspace-only (not
> available to personal `@gmail.com` accounts) and typically requires
> service-account / domain-wide-delegation auth rather than the OAuth-refresh
> flow this package uses everywhere else.

## Quick start: log in once, then use `get-google-doc` / `post-google-doc`

The package ships a one-shot `login-to-google` Script action that runs
the entire OAuth 2.0 authorization-code choreography through the
dispatcher: bind the loopback listener, open the system browser (via the
new `/builtin/open_url` internal action), wait for the callback, exchange
the code for tokens, persist the credentials via `/builtin/set_secret`
(encrypted with the key configured in this action's own
`action_config.secrets` map), and release the port. After it returns
successfully, `get-google-doc` and `post-google-doc` work without any
further setup.

```sh
# from sol-browser/
solx exec /packages/solx-google/login-to-google --json '{
  "client_id": "YOUR_GOOGLE_OAUTH_CLIENT_ID.apps.googleusercontent.com",
  "client_secret": "YOUR_GOOGLE_OAUTH_CLIENT_SECRET",
  "scope": "https://www.googleapis.com/auth/documents https://www.googleapis.com/auth/drive.file https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/gmail.send https://www.googleapis.com/auth/tasks https://www.googleapis.com/auth/calendar.events"
}'
```

Only request the scopes you actually need. This opens your default system
browser on the Google consent screen. Sign in, approve the requested
scopes, and the script continues.

On subsequent runs you can omit the params entirely — the script reads
them back via `/builtin/get_secret`:

```sh
solx exec /packages/solx-google/login-to-google
```

After login, you can run:

```sh
solx exec /packages/solx-google/get-google-doc --json '{ "documentId": "1AbC...XYZ" }'
```

To revoke the session, run `login-to-google` again — it overwrites the
stored `refresh_token` (the previous one stops being valid on the
provider side after a fresh consent with `prompt=consent`).

## Install

```sh
# from sol-browser/
solx install-package ../sol-packages/solx-google
```

This:

1. Generates a fresh random 32-byte encryption key (`/builtin/random_string`).
2. Posts 35 JSON-schema types under `/packages/solx-google/`.
3. Uploads the login script content via `/builtin/file_put` at
   `shared/solx-google-login.solx` (the script body is inlined into
   `install.solx` — no separate template / build step required).
4. Posts the `login-to-google` Script action pointing at the uploaded
   file.
5. Posts 14 webhook actions (Docs, Drive, Gmail, Tasks, Calendar) plus
   the `oauth-token-exchange` action — each with the same encryption key
   baked into its `action_config.secrets` map.

The login script and WASM binary are stored as files in the file store;
no `artifact` registry is involved.

## What this package registers

- **35 types** under `/packages/solx-google/`
- **16 actions**: 14 webhooks (Docs/Drive/Gmail/Tasks/Calendar) +
  1 oauth-token-exchange webhook + 1 login-to-google script action
- **1 file**: `shared/solx-google-login.solx`

## WASM converters

The 8 Sol ↔ Google converter actions are hosted in a single WASM
component (`solx-google-actions`), dispatched by `fn_name`:

- `convert-sol-doc-to-google-doc`
- `convert-google-doc-to-sol-doc`
- `convert-sol-doc-to-gmail-message`
- `convert-gmail-message-to-sol-doc`
- `convert-sol-doc-to-google-task`
- `convert-google-task-to-sol-doc`
- `convert-sol-doc-to-calendar-event`
- `convert-calendar-event-to-sol-doc`

The Tiptap-to-Google-Docs converter is the most complex piece — it does
a two-pass walk emitting `insertText` requests first, then
`updateTextStyle`/`updateParagraphStyle`/`createParagraphBullets` style
mutations in the right order.

### Posting the WASM actions

After running `cargo build --release --target wasm32-wasip2`, upload the
binary via `file_put` and post each action manually (these are NOT in
`install.solx` to keep it readable). Example:

```sh
# 1. Upload the binary
solx exec /builtin/file_put --json '{
  "rel_path": "shared/solx-google-actions.wasm",
  "content_base64": "...",  # base64 of the .wasm bytes
  "encoding": "base64"
}'

# 2. Post each action
solx post action /packages/solx-google/convert-sol-doc-to-google-doc \
  --json '{
    "action_type": "wasm",
    "bin_name": "solx-google-actions.wasm",
    "caption": "Convert Sol document to Google Docs payload",
    "param_type_ref": "/packages/solx-google/SolDocToGoogleDocParams",
    "result_type_ref": "/packages/solx-google/SolDocToGoogleDocResult"
  }'
# (repeat for the other 7)
```

## Security

OAuth secrets are encrypted and stored in the scoped secret store,
scoped so that only the actions this package registers — which share a
single randomly generated key baked into their `action_config.secrets`
maps at install time — can decrypt them. The encryption key never leaves
the action's `action_config` (which itself is redacted on `get`).

## Differences from old `sol-google`

| Old (`sol-google`) | New (`solx-google`) |
|---|---|
| `action_type: "Actions"` for login | `action_type: "script"` (solx has no `Actions` type) |
| Built-in `open system browser` action | New `/builtin/open_url` internal action (added in solx-core) |
| Login script as a 14-step ActionScript JSON file | Login script as a `script`-typed action pointing at `login.solx` |
| WASM converters in `sol-google-actions.wasm` (Python + Rust dispatch) | Same converter logic, ported to Rust in `solx-google-actions` |
| `bin_name: "shared::sol-google-actions.wasm"` | `bin_name: "solx-google-actions.wasm"` (file store convention, no `shared::` prefix) |
| `Links` + `action-links` entities | Skipped (solx-core has no link entities; they were purely organizational) |
| Artifacts for `sol-google-actions.wasm` and `sol-google-login-script.json` | Stored as files in the file store instead |
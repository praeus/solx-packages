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
new `/builtin/web/open_url` internal action), wait for the callback, exchange
the code for tokens, persist the credentials via `/builtin/secrets/set_secret`
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
them back via `/builtin/secrets/get_secret`:

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

Build the WASM converter binary first — `install.solx` reads
`bin/solx-google-actions.wasm` as its first statement:

```sh
# from solx-google/
./build.sh          # or build.ps1 on Windows
```

Then install:

```sh
# from sol-browser/
solx install-package ../solx-packages/solx-google
```

This:

1. Uploads `bin/solx-google-actions.wasm` (built above) to the file store
   at `shared/solx-google-actions.wasm`.
2. Generates a fresh random 32-byte encryption key (`solx random 32`).
3. Posts 35 JSON-schema types under `/packages/solx-google/`.
4. Uploads the login script content via `/builtin/file/file_put` at
   `shared/solx-google-login.solx` (the script body is inlined into
   `install.solx` — no separate template / build step required).
5. Posts the `login-to-google` Script action pointing at the uploaded
   file.
6. Posts 14 webhook actions (Docs, Drive, Gmail, Tasks, Calendar) plus
   the internal `_private/oauth-token-exchange` action — each with the same
   encryption key baked into its `action_config.secrets` map.
7. Posts 3 WASM actions built on the Docs converter (see below):
   `convert-sol-doc-to-google-doc`, `convert-google-doc-to-sol-doc`, and
   `upload-documents-to-google-docs`.

The login script and WASM binary are stored as files in the file store;
no `artifact` registry is involved.

## What this package registers

- **35 types** under `/packages/solx-google/`
- **19 actions**: 14 webhooks (Docs/Drive/Gmail/Tasks/Calendar) +
  1 internal `_private/oauth-token-exchange` webhook (not meant to be
  invoked standalone) + 1 login-to-google script action + 3 WASM Docs
  converter actions
- **2 files**: `shared/solx-google-login.solx`, `shared/solx-google-actions.wasm`

## WASM converters

The 8 Sol ↔ Google converter actions plus the batch uploader are hosted
in a single WASM component (`solx-google-actions`), dispatched by
`fn_name`:

- `convert-sol-doc-to-google-doc` — installed
- `convert-google-doc-to-sol-doc` — installed
- `upload-documents-to-google-docs` — installed
- `convert-sol-doc-to-gmail-message`
- `convert-gmail-message-to-sol-doc`
- `convert-sol-doc-to-google-task`
- `convert-google-task-to-sol-doc`
- `convert-sol-doc-to-calendar-event`
- `convert-calendar-event-to-sol-doc`

Only the three Docs-related ones (marked "installed" above) are posted
by `install.solx` — the Gmail/Tasks/Calendar converters below still need
posting by hand.

The Tiptap-to-Google-Docs converter is the most complex piece — it does
a two-pass walk emitting `insertText` requests first, then
`updateTextStyle`/`updateParagraphStyle`/`createParagraphBullets` style
mutations in the right order.

`upload-documents-to-google-docs` is a batch action built on that same
converter: given a `path_prefix` and a `count`, it lists Sol documents
under that path via `/builtin/document/entity_list_documents` (sorted by
`updated_at`, `order: "asc"|"desc"`, default `desc`, skipping an optional
`offset` first, default `0`, so successive runs can page through a
folder), converts each
document in-process (the same code `convert-sol-doc-to-google-doc` uses,
factored into a shared `build_google_doc_payload` helper — no extra
`action-exec` hop through the standalone converter action), then either
creates or updates a Google Doc for each one (see below). It's fail-fast:
on the first failure it stops and returns `success: false` naming the
failing document, with `uploaded` still listing whatever succeeded before
that.

**Re-running it doesn't create duplicates.** Every created Google Doc is
tagged with the source Sol document's path via Drive's `appProperties`
(`GoogleDriveCreateFileParams.appProperties`, an official, queryable Drive
file property — not a Sol-side mapping to keep in sync). Before creating,
it searches Drive for a file already tagged with that path
(`find-google-drive-folder` with an `appProperties has {...}` query); if
found, it clears the existing doc's body (a `deleteContentRange` covering
everything but the final implicit newline, computed from a `get-google-doc`
read) and reposts fresh content into the *same* doc instead of making a
new one. Each `uploaded[]` entry's `action` field says which happened
(`"created"` or `"updated"`). This is self-healing: if you delete the
Google Doc yourself, the tag search just finds nothing and a fresh one
gets created and re-tagged, no stale reference left behind.

Documents from *before* this tagging existed aren't tagged retroactively —
the next run over one of those creates one more doc (now tagged); every
run after that updates it in place.

A post's icon is best-effort: it's hotlinked from Drive (upload +
public-share), and `insertInlineImage` is a synchronous fetch by Google's
own servers with no auth context, which is known to be flaky (an
unindexed-yet Drive thumbnail 404s, an HTML interstitial instead of raw
bytes, etc.). Since `post-google-doc` sends the whole document as one
atomic `batchUpdate`, a bad icon would otherwise take the entire
document's text and formatting down with it. If the first `batchUpdate`
fails specifically on `insertInlineImage`, the document is rebuilt
without the icon and resubmitted once before the document counts as
failed.

### Posting the remaining WASM actions (Gmail/Tasks/Calendar)

The binary is already uploaded by `install.solx` (step 1 above). Post
each remaining converter manually — these are NOT in `install.solx`, to
keep it from growing to cover every converter nobody may end up using.
Example:

```sh
solx save action /packages/solx-google/convert-sol-doc-to-gmail-message \
  --json '{
    "actionType": "wasm",
    "binName": "solx-google-actions.wasm",
    "fnName": "convert-sol-doc-to-gmail-message",
    "caption": "Convert Sol document to Gmail message",
    "paramTypeRef": "/packages/solx-google/SolDocToGmailParams",
    "resultTypeRef": "/packages/solx-google/SolDocToGmailResult"
  }'
# (repeat for the other 5 Gmail/Tasks/Calendar converters, using their
# matching fnName + paramTypeRef/resultTypeRef pairs from install.solx's
# type definitions)
```

Example usage of the installed batch uploader:

```sh
solx exec /packages/solx-google/upload-documents-to-google-docs --json '{
  "path_prefix": "/notes",
  "count": 5,
  "offset": 0,
  "order": "desc"
}'
```

## Security

OAuth secrets are encrypted and stored in the scoped secret store,
scoped so that only the actions this package registers — which share a
single randomly generated key baked into their `action_config.secrets`
maps at install time — can decrypt them. The encryption key never leaves
the action's `action_config` (which itself is redacted on `get`).

## Uninstall

```sh
solx uninstall-package solx-google
```

Deletes every action, type, and file `install.solx` registered (order
doesn't matter — no cross-DB foreign keys). There's no built-in
`delete_secret` primitive, so OAuth secrets aren't deleted directly:
`set_secret`/`get_secret` are scoped to the calling action's own
encryption key (declared in each OAuth action's `action_config.secrets`
map), so once the owning action row is gone, its secrets are unreachable
and the scoped store garbage-collects them — the `delete action` calls
above trigger that path, no separate step needed.

## Differences from old `sol-google`

| Old (`sol-google`) | New (`solx-google`) |
|---|---|
| `action_type: "Actions"` for login | `action_type: "script"` (solx has no `Actions` type) |
| Built-in `open system browser` action | New `/builtin/web/open_url` internal action (added in solx-core) |
| Login script as a 14-step ActionScript JSON file | Login script as a `script`-typed action pointing at `login.solx` |
| WASM converters in `sol-google-actions.wasm` (Python + Rust dispatch) | Same converter logic, ported to Rust in `solx-google-actions` |
| `bin_name: "shared::sol-google-actions.wasm"` | `bin_name: "solx-google-actions.wasm"` (file store convention, no `shared::` prefix) |
| `Links` + `action-links` entities | Skipped (solx-core has no link entities; they were purely organizational) |
| Artifacts for `sol-google-actions.wasm` and `sol-google-login-script.json` | Stored as files in the file store instead |
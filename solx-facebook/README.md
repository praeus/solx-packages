# solx-facebook

Facebook Page posts for solx-core over the Graph API (v25.0): get, create,
update, and list posts with cursor pagination. Uses native Webhook actions
that call `https://graph.facebook.com/...` directly, authenticated with a
Page access token encrypted and persisted to the scoped secret store.

> **No keyword search.** The Graph API has no text search over Page posts.
> `list-facebook-posts` narrows by date (`since`/`until`) and paginates;
> filter by content on the caller side.

## Prerequisites

A Meta app (developers.facebook.com → My Apps) with the **Facebook Login**
product added, and a Page you administer. The actions need these
permissions: `pages_show_list`, `pages_read_engagement`,
`pages_read_user_content` (commenter names, for imports), and
`pages_manage_posts`. While the app is in Development mode this works for
anyone with a role on the app; publishing for other users needs App Review.

For the OAuth login below, add `http://127.0.0.1:8765/callback` (or your
chosen `port`) to **Facebook Login → Settings → Valid OAuth Redirect URIs**.

## Quick start: log in once

`login-to-facebook` supports two modes.

**OAuth (recommended):** opens the Facebook Login dialog in your browser,
exchanges the code for a long-lived user token, and stores the access token
of one of the Pages you manage. Page tokens obtained this way don't expire.

```sh
solx exec /packages/solx-facebook/login-to-facebook --json '{
  "app_id": "YOUR_META_APP_ID",
  "app_secret": "YOUR_META_APP_SECRET",
  "page_id": "OPTIONAL_PAGE_ID"
}'
```

Omit `page_id` to use the first Page returned by `/me/accounts`. The app
id/secret are persisted, so later runs (e.g. to switch Pages) can omit
them.

**Manual:** paste a Page access token you already have (e.g. from the
Graph API Explorer, "Get Page Access Token"). It is validated against
`GET /me` and stored. Tokens from the Explorer are short-lived unless you
extend them first.

```sh
solx exec /packages/solx-facebook/login-to-facebook --json '{ "page_access_token": "EAAB..." }'
```

Both modes return `{"succeeded": true, "page": {"id": "...", "name": "..."}}`.

### Troubleshooting login

- **`check-app-credentials failed ... Error validating application`**: Facebook
  doesn't recognize `app_id`. (Without this check you'd get the login
  dialog's "Invalid App ID" page.) Use the **App ID** from App settings →
  Basic of an app that has the Facebook Login product, not a Business
  portfolio ID, a Login for Business configuration ID, or an Instagram app
  ID.
- **`... Error validating client secret`**: `app_secret` doesn't match the app
  (App settings → Basic → App secret → Show).
- **"URL blocked" / redirect URI errors in the browser**: add
  `http://127.0.0.1:8765/callback` to Facebook Login → Settings → Valid
  OAuth Redirect URIs, and make sure Client OAuth login and Web OAuth login
  are on.
- **`failed to bind oauth loopback`**: another process holds the port. Pass
  a different `port` (and register that redirect URI too). A listener left
  over from an abandoned login in solx itself is taken over automatically.

## Actions

All post actions act as the connected Page (`/me` resolves to the Page when
called with a Page token), so no `page_id` is passed.

| Action | Endpoint |
| --- | --- |
| `get-facebook-post` | `GET /{post_id}` |
| `create-facebook-post` | `POST /me/feed` |
| `update-facebook-post` | `POST /{post_id}` (only posts created by this app) |
| `list-facebook-posts` | `GET /me/posts` |

```sh
# Create (returns {"id": "<page_id>_<post_id>"})
solx exec /packages/solx-facebook/create-facebook-post --json '{ "message": "Hello from solx", "link": "https://example.com" }'

# Schedule (10 minutes to 30 days ahead)
solx exec /packages/solx-facebook/create-facebook-post --json '{ "message": "Later", "published": false, "scheduled_publish_time": "2026-10-01T09:00:00Z" }'

# Get
solx exec /packages/solx-facebook/get-facebook-post --json '{ "post_id": "123_456", "fields": "id,message,created_time,permalink_url" }'

# List, newest first, 10 per page, created in 2026
solx exec /packages/solx-facebook/list-facebook-posts --json '{ "fields": "id,message,created_time", "limit": 10, "since": "2026-01-01" }'

# Next (older) page: pass paging.cursors.after from the previous response
solx exec /packages/solx-facebook/list-facebook-posts --json '{ "fields": "id,message,created_time", "limit": 10, "since": "2026-01-01", "after": "QVFIU..." }'
```

A list response with no `paging.next` is the last page. Without `fields`,
the Graph API returns only `id`, `message`, and `created_time`.

`list-facebook-comments` (`GET /{object_id}/comments`) pages through a
post's comments, or a comment's replies, the same way.

## Importing posts as Sol documents (WASM)

`solx-facebook-actions.wasm` hosts three actions that turn Page posts into
`BlogPostWithComments` documents, the same type solx-livejournal produces:

| Post | Document |
| --- | --- |
| `message` (or `story`) | `contents.content` (Tiptap), `text`, `paragraphs`; first line → `title` |
| shared-link attachment | trailing link paragraph + a `links` entry |
| comments and their replies | `contents.comments` (author, text, date, nested `replies`) |
| `full_picture` | downloaded to `files/docs/shared/fb-<post_id>-picture.<ext>` → `contents.icon` |
| `permalink_url`, ids, timestamps | `links`, `pubDate`, `contents.source` |

Documents are saved under `/blogs/facebook/<page-name-slug>` (override
with `path`) with the post ID as the name, so re-importing a post updates
its document instead of duplicating it.

```sh
# One post, with its comment thread
solx exec /packages/solx-facebook/import-facebook-post --json '{ "post_id": "123_456" }'

# The 50 newest posts from 2026. Pass the result's next_after as "after" to continue.
solx exec /packages/solx-facebook/import-facebook-posts --json '{ "count": 50, "since": "2026-01-01" }'

# Pure conversion, no network and nothing saved: returns sol_document_payload,
# which can be passed straight to /builtin/document/entity-save-document
solx exec /packages/solx-facebook/convert-facebook-post-to-sol-doc --json '{ "facebook_post": { "id": "123_456", "message": "Hi" } }'
```

Notes:

- **Commenter names** need `pages_read_user_content` (in the default login
  scope). Without it Facebook omits `from` and comments import with
  `author: null`.
- **Comment cap:** `max_comments` (default 500) limits comments + replies
  per post. `comments_truncated` in the result says whether it cut off.
- **Pictures are best-effort.** Facebook serves them from regional CDN hosts
  (e.g. `https://scontent.fsyd3-1.fna.fbcdn.net/`), which are not in this
  package's `allowed_base_urls`. Add your region's host prefix to
  `solx-config.json`'s `allowed_base_urls` if you want icons. Otherwise the
  download is skipped with a console log line and the post imports without
  an icon.
- **Batch failures don't stop the batch:** `import-facebook-posts` lists
  failed posts in `failed` (and reports `success: false`), and honors
  `action-stop` between posts.

## What this package registers

- **15 types** under `/packages/solx-facebook/`
- **13 actions**: 5 webhooks (posts + comments), 3 WASM import/convert
  actions, the `login-to-facebook` script, and 4 internal `_private/*`
  login steps (`oauth-token-exchange`, `exchange-long-lived-token`,
  `list-managed-pages`, `get-token-identity`) that aren't meant to be
  invoked standalone
- **2 files**: `files/actions/shared/solx-facebook-login.solx`,
  `files/actions/shared/solx-facebook-actions.wasm`

`install.solx` uploads `login.solx` as-is with
`save file ... --file login.solx`, so edits to `login.solx` take effect on
the next install with no regeneration step.

## Install / uninstall

Build the WASM binary first: `install.solx` reads
`bin/solx-facebook-actions.wasm` as its first statement (needs
`rustup target add wasm32-wasip2`, and solx-core checked out next to
solx-packages).

```sh
./build.sh          # or build.ps1 on Windows; --install / -Install also installs
solx install-package ../solx-packages/solx-facebook
solx uninstall-package solx-facebook
```

Reinstalling keeps the stored token: the encryption key is reused when any
action under `/packages/solx-facebook` already exists.

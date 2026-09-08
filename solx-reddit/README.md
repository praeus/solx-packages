# solx-reddit

Search Reddit, or extract one post directly, into `BlogPostWithComments`
documents with a bounded slice of the comment tree.

One quickjs wasm component backs two actions, dispatched on `fn_name`:

| Action | `fn_name` | Does |
| --- | --- | --- |
| `reddit-extract-post` | `extract_post` | One post → one document. |
| `reddit-search` | `search_posts` | A search (optionally scoped to one subreddit) → up to `limit` documents. Returns an `after` cursor for the next page. |

## Requirements

- The `solx-firefox` and `solx-mcp-actions` packages installed. Every
  `reddit-*` action calls `/packages/solx-firefox/firefox-start` before
  touching the page, which launches the managed, dedicated-profile Firefox
  (Marionette enabled) if it isn't already running, or reuses it if it is.
- **The managed Firefox profile should be signed in to Reddit.** This is not
  optional the way it is for `solx-livejournal`: Reddit redirects *every*
  logged-out request — search, subreddit listings, individual posts — to a
  login wall tagged `reason=lor2`, observed from plain server-side HTTP
  clients regardless of `User-Agent`. A real, signed-in Firefox session was
  the only thing that got past it in testing. A signed-out run fails loudly
  with a "redirected to the Reddit login page" error rather than silently
  returning nothing, so a bad session is obvious immediately. Sign in once in
  the managed profile and it persists across restarts.
- The `/types/docs/BlogPostWithComments` type (ships with solx-core).

Every `reddit-*` action navigates the managed Firefox tab to
`https://old.reddit.com/` before touching the page (in addition to
`firefox-start`), so no manual navigation step is needed even on a freshly
launched, `about:blank` Firefox.

**Why old.reddit.com, not the JSON API.** Reddit's `.json` endpoints
(`/search.json`, `/comments/<id>.json`, …) were tried first and are blocked
outright (HTTP 403) independent of the login wall — that block held even from
inside tests with a full browser `User-Agent` and headers. old.reddit.com
still serves plain, server-rendered HTML with long-stable markup, which is
what the page scripts parse with `DOMParser`, the same technique
`solx-livejournal` uses.

## Usage

Extract one post:

```
solx exec /packages/solx-reddit/reddit-extract-post --json '{"url":"https://www.reddit.com/r/dndnext/comments/abc123/some_title/","maxComments":30}'
```

`subreddit` + `id` work in place of `url`. Documents land in
`/blogs/reddit/<subreddit>` (override with `path`), named
`<post-id>-<title-slug>` — re-running upserts rather than duplicating.

Search and save a batch:

```
solx exec /packages/solx-reddit/reddit-search --json '{"query":"dungeons and dragons","subreddit":"dndnext","limit":10,"maxComments":20}'
```

`subreddit` restricts the search to one community; omit it to search all of
Reddit. `after` (a post fullname like `t3_abc123`, returned in the previous
call's result) fetches the next page of search results — there is no stored
cursor the way `solx-livejournal`'s `lj-harvest` has one, since a search
isn't a fixed sequence the way an index-page walk is.

## Field mapping

| Document | Source |
| --- | --- |
| `title` | `a.title` on the post's `.thing` row |
| `contents.text`, `contents.paragraphs` | self-post body (`.usertext-body .md`), block elements and `<br>` both normalised to paragraphs; a link post's target URL is prepended as its own paragraph |
| `contents.content` | the same paragraphs as a ProseMirror-shaped `RichTextDoc` |
| `contents.icon` | the post's thumbnail image, downloaded and stored as a `files/docs/shared/...` relPath — absent when the post has none or the download fails |
| `contents.comments` | recursive `{author, icon, text, date, replies}`, capped at `maxComments` nodes counted across the *whole* tree (not per level) — `icon` is always `null`, since old.reddit's comment rows don't carry a per-commenter avatar |
| `author` | the post's author |
| `pub_date` | the post's `<time datetime="...">` attribute, already ISO-8601 |
| `summary` | first paragraph, truncated to 300 chars |
| `links` | the permalink, a link to the subreddit, and one `field:"tags"` link for the post's flair, if any |

## How it works, and why

**`maxComments` is a total budget, not a per-thread limit.** `shapeComment`
decrements a shared counter on every node — top-level and nested alike — and
once it hits zero every remaining call, including siblings deeper in the
walk, returns `null` and is filtered out. This is what "fill in a certain
number of the comments" means literally: the *n*th comment saved could be a
reply three levels deep, not necessarily the *n*th top-level thread.

**Errors are not retried past the first try.** `solx-livejournal`'s
`evaluateInPage` gates its retry loop on the page script's result carrying a
particular key (e.g. `entries`), so a page script's own `{error: ...}` result
looks like "nothing yet" and gets retried up to four times before the wrapped
generic failure is thrown. Reddit's login wall is exactly the kind of error
that call would waste three retries rediscovering, so this package's
`evaluateInPage` accepts *any* parsed result immediately — success or a
structured `{error: ...}` — and only retries when the evaluate call itself
fails (a dropped BiDi connection, an empty result).

**One post per browser call**, for the same reason `solx-livejournal` does
this: `firefox-devtools-mcp` hardcodes a 10s BiDi per-command timeout and
ignores the `timeout` argument, so batching several posts into one
`evaluate_script` call reliably fails partway.

## Install

```
solx install-package .
```

`install.solx` stages `src/solx-reddit.js` into the file store, then calls
`solx-quickjs`'s `build-javascript-file` action to compile it to a wasm
component and upload it to `files/actions/shared/solx-reddit.wasm` — the same
path the `reddit-*` actions' `bin_name` points at — before registering the
types and actions.

This means `solx-quickjs` must already be installed, with its CLI built at
`solx-quickjs/target/release/solx-quickjs.exe`, and `solx-server` must be
running (the build action talks to it over HTTP) before installing this
package — see `../solx-quickjs/README.md`.

## Verify

```
solx script -f verify.solx
```

Exercises both actions: `reddit-extract-post` against a long-lived, stable
post (`r/announcements`'s original "gasp" post), and `reddit-search` for a
handful of `r/dndnext` results. If either comes back with a "redirected to
the Reddit login page" error, sign in to Reddit in the managed Firefox
profile and re-run.

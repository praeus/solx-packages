# solx-livejournal

Extract a LiveJournal into `BlogPostWithComments` documents, with the full
comment tree, resumably.

One quickjs wasm component backs three actions, dispatched on `fn_name`:

| Action | `fn_name` | Does |
| --- | --- | --- |
| `lj-extract-entry` | `extract_entry` | One entry → one document. |
| `lj-harvest-page` | `harvest_page` | One index page → N documents. Returns the page's `prev` link. |
| `lj-harvest` | `harvest` | Resumable loop over index pages, cursor held in the env store. |

## Requirements

- A running Firefox reachable through the `solx-firefox` / `solx-mcp-actions`
  packages, i.e. the `/mcp-firefox-evaluate-script` action must execute.
- **The journal must be signed in in that browser.** Everything runs as
  same-origin `fetch(..., {credentials:'include'})` from a tab on the journal's
  own origin, so the extraction sees exactly what the signed-in user sees.
  Signed in as the journal owner you get private, friends-locked, and screened
  content; signed out you silently get only public entries and the run still
  reports success. The `security` field on each saved entry (`public`,
  `friends`, `private`) is the quickest way to confirm you got the locked ones.
- The `/types/docs/BlogPostWithComments` type (ships with solx-core).

Point the browser at the journal before the first run:

```
solx exec /mcp-firefox-navigate-page --json '{"url":"https://<user>.livejournal.com/"}'
```

## Usage

```
solx exec /packages/solx-livejournal/lj-harvest --json '{"user":"<user>","max_pages":5}'
```

Documents land in `/blogs/livejournal/<user>` (override with `path`), named
`<entry-id>-<title-slug>`. The name is derived from the entry id, so re-running
upserts rather than duplicating — a re-run over already-harvested pages is safe
and simply refreshes them (picking up new comments).

Resume state lives in the env store under namespace `livejournal`, one key per
journal (both overridable with `namespace` / `cursor_key`). It is written with
`persist: true`, so it lands in `solx-config.json` under `env_vars` and survives
a solx-server restart. The cursor advances **after each completed page**, so an
interrupted run resumes at the first page it did not finish. With no cursor set,
the harvest starts at page one.

```
solx exec /builtin/env/get_env --json '{"namespace":"livejournal","key":"<user>"}'
solx exec /builtin/env/set_env --json '{"namespace":"livejournal","key":"<user>","value":"/"}'
```

To clear the cursor entirely and start over, delete the entry from `env_vars` in
`solx-config.json`. Re-running over already-harvested pages is harmless either
way — saves are upserts, so it just refreshes those entries.

## Field mapping

| Document | Source |
| --- | --- |
| `title` | `.b-singlepost-title` / `.aentry-post__title`, falling back to `og:title` |
| `contents.text`, `contents.paragraphs` | entry body, block elements and `<br><br>` both normalised to paragraphs |
| `contents.content` | the same paragraphs as a ProseMirror-shaped `RichTextDoc`, which is what solx's rich-text indexer walks |
| `contents.comments` | recursive `{author, text, date, replies}` |
| `author` | the journal username |
| `pub_date` | the entry's `<time>` text |
| `summary` | first paragraph, truncated to 300 chars |
| `links` | the permalink, plus one `field:"tags"` link per tag |

## How it works, and why

**Comments come from an internal JSON-RPC, not the DOM.** `?format=light`
server-renders the post body but injects comments with Angular, so a fetched
document contains zero comments. The page script instead calls:

```js
LJ.Api.callP('comment.get_thread', { journal, itemid, expand_all: 1 })
```

Each comment carries an explicit `parent` pointer, so the tree is reconstructed
exactly rather than inferred from indentation, and `expand_all: 1` defeats
thread collapsing. This is why the package needs no LLM.

**One entry per browser call.** `firefox-devtools-mcp` hardcodes a 10s BiDi
per-command timeout and ignores the `timeout` argument, so batching several
entries into a single `evaluate_script` reliably fails partway. Each entry gets
its own call, at roughly 4-6s.

**Two template generations.** A long-lived journal serves both the older
`.b-singlepost-*` markup and the newer `.aentry-post__*`; the extractor tries
both. Tags are read via `a[href*="/tag/"]`, which is stable across both.

**Retries live at the call level.** When the BiDi connection drops, every fetch
inside that one script run fails together, so retrying inside the page is
useless — a *fresh* evaluate call is what recovers. `evaluateInPage` retries
four times.

## Build

```
./build.sh          # or ./build.ps1
./build.sh --install
```

`build-javascript-action` from `solx-quickjs` must be installed and its CLI
built first.

## Verify

`verify.solx` exercises all three actions against a real journal. Edit the
username first, then:

```
solx script -f verify.solx
```

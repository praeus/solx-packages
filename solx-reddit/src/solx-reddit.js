// solx-reddit — extract Reddit posts into BlogPostWithComments documents.
//
// One wasm component backing two actions, dispatched on `fn_name`:
//   extract_post  — one post -> one document
//   search_posts  — a search (optionally scoped to one subreddit) -> N documents
//
// Everything runs through the managed Firefox session against old.reddit.com,
// same-origin `fetch(..., {credentials:'include'})`, exactly like
// solx-livejournal. Unlike LiveJournal, this isn't about reaching
// friends-locked content: Reddit redirects logged-out requests on *every*
// path (search, subreddit listings, even individual posts) to a login wall
// tagged `reason=lor2`, and does this from server-side HTTP clients
// regardless of User-Agent. A real, signed-in Firefox session is the only
// combination observed to get past it — sign in once in the managed profile
// (see README) and it persists across restarts. A signed-out session fails
// loudly with a "login wall" error rather than silently returning nothing.
//
// old.reddit.com is used instead of www.reddit.com because it still serves
// plain server-rendered HTML (stable markup, parseable with DOMParser) where
// the modern UI is a client-rendered app. Reddit's own `.json` API endpoints
// were tried and are blocked outright (403) independent of the login wall.

import { exec } from "sol:actions/action-exec@0.1.0";
import { log } from "sol:actions/logger@0.1.0";

const FIREFOX_EVAL = "/packages/solx-mcp-actions/firefox/mcp-firefox-evaluate-script";
const FIREFOX_START = "/packages/solx-firefox/firefox-start";
const FIREFOX_NAVIGATE = "/packages/solx-mcp-actions/firefox/mcp-firefox-navigate-page";
const TYPE_REF = "/types/docs/BlogPostWithComments";

// The firefox-devtools-mcp BiDi transport hardcodes a 10s per-command timeout
// and ignores the `timeout` param (see solx-livejournal for the source of
// this note), so each page script must stay well under it — one post/search
// per call rather than batching.
const EVAL_TRIES = 4;

// ── host helpers ───────────────────────────────────────────────────────────

function callExec(ref, params) {
  const r = exec(ref, JSON.stringify(params || {}));
  if (!r || !r.success) {
    throw new Error(ref + ": " + ((r && r.message) || "action failed"));
  }
  return r.output ? JSON.parse(r.output) : null;
}

function isCancelled() {
  try {
    const r = callExec("/builtin/action/cancelled", {});
    return !!(r && r.cancelled);
  } catch (e) {
    return false;
  }
}

// Pull the script's return value back out of the MCP text envelope, which
// wraps it in a ```json fence.
function unwrapEval(output) {
  const content = output && output.content;
  const text = content && content[0] && content[0].text;
  if (!text) return null;
  const start = text.indexOf("```json");
  if (start === -1) return null;
  const body = text.slice(start + 7);
  const end = body.lastIndexOf("```");
  return JSON.parse(end === -1 ? body : body.slice(0, end));
}

let firefoxReady = false;

function ensureFirefox() {
  if (firefoxReady) return;
  callExec(FIREFOX_START, {});
  callExec(FIREFOX_NAVIGATE, { url: "https://old.reddit.com/" });
  firefoxReady = true;
}

// A dropped BiDi connection fails every fetch inside one script run, but a
// fresh evaluate call recovers — so retry at this level, not inside the page.
//
// Unlike solx-livejournal's `evaluateInPage`, this does not gate on a
// "wanted key": a page script's own `{error: ...}` result is a legitimate,
// non-retryable answer (a login wall, a missing post), and treating it as
// "keep retrying" would hammer Reddit four times before surfacing an error
// that was already known on the first try. Only an outright eval failure
// (dropped connection, empty result) is retried.
function evaluateInPage(js) {
  let last = null;
  for (let i = 0; i < EVAL_TRIES; i++) {
    try {
      const out = unwrapEval(callExec(FIREFOX_EVAL, { function: js, timeout: 30000 }));
      if (out) return out;
      last = "empty result";
    } catch (e) {
      last = String(e).slice(0, 300);
    }
  }
  throw new Error("page script failed after " + EVAL_TRIES + " tries: " + last);
}

function loginWallError() {
  return new Error(
    "redirected to the Reddit login page (reason=lor2) — sign in to Reddit in the managed Firefox profile first"
  );
}

// ── page scripts (run inside the Firefox tab) ───────────────────────────────

// Shared prelude: a retrying same-origin fetch that carries the session and
// recognizes Reddit's logged-out login-wall redirect as a distinct outcome
// from a genuine HTTP failure.
const FETCH_HELPER = [
  "const fetchHtml = async (url, tries) => {",
  "  let last;",
  "  for (let i = 0; i < (tries || 2); i++) {",
  "    let r;",
  "    try { r = await fetch(url, { credentials: 'include', redirect: 'follow' }); }",
  "    catch (e) { last = String(e); await new Promise(s => setTimeout(s, 400)); continue; }",
  "    if ((r.url || '').indexOf('/login') !== -1) return { loginWall: true };",
  "    if (r.ok) return { html: await r.text() };",
  "    last = 'HTTP ' + r.status;",
  "    await new Promise(s => setTimeout(s, 400));",
  "  }",
  "  return { fetchError: last };",
  "};"
].join("\n");

// Splits a rich-text block element's rendered text into paragraphs the same
// way solx-livejournal does: <br> -> newline, block boundaries -> a blank
// line, then split on blank lines.
const PARA_HELPER = [
  "const htmlToParas = el => {",
  "  if (!el) return [];",
  "  const c = el.cloneNode(true);",
  "  c.querySelectorAll('br').forEach(b => b.replaceWith('\\n'));",
  "  c.querySelectorAll('p,div,blockquote,li,h1,h2,h3,h4,pre').forEach(b => b.append('\\n'));",
  "  return (c.textContent || '').replace(/\\u00a0/g, ' ')",
  "    .split(/\\n\\s*\\n+/).map(s => s.replace(/[ \\t]+/g, ' ').trim()).filter(Boolean);",
  "};"
].join("\n");

function searchScript(query, subreddit, sort, time, after) {
  return [
    "async () => {",
    "  const Q = " + JSON.stringify(query) + ";",
    "  const SUB = " + JSON.stringify(subreddit || null) + ";",
    "  const SORT = " + JSON.stringify(sort) + ";",
    "  const TIME = " + JSON.stringify(time) + ";",
    "  const AFTER = " + JSON.stringify(after || null) + ";",
    FETCH_HELPER,
    "  const path = SUB ? ('/r/' + SUB + '/search') : '/search';",
    "  let qs = '?q=' + encodeURIComponent(Q) + '&sort=' + SORT + '&t=' + TIME + (SUB ? '&restrict_sr=on' : '');",
    "  if (AFTER) qs += '&after=' + encodeURIComponent(AFTER);",
    "  const res = await fetchHtml(path + qs, 3);",
    "  if (res.loginWall) return { error: 'login_wall' };",
    "  if (res.fetchError) return { error: res.fetchError };",
    "  const doc = new DOMParser().parseFromString(res.html, 'text/html');",
    "  const seen = {}; const posts = []; let lastFullname = null;",
    "  doc.querySelectorAll('.search-result-link').forEach(el => {",
    "    const fullname = el.getAttribute('data-fullname') || '';",
    "    const a = el.querySelector('a.search-title');",
    "    if (!a) return;",
    "    const href = a.getAttribute('href') || '';",
    "    const m = href.match(/\\/r\\/([^\\/]+)\\/comments\\/([a-z0-9]+)/i);",
    "    if (!m || seen[m[2]]) return;",
    "    seen[m[2]] = 1;",
    "    posts.push({ id: m[2], subreddit: m[1], title: (a.textContent || '').trim() });",
    "    if (fullname) lastFullname = fullname;",
    "  });",
    "  return { posts: posts, after: lastFullname };",
    "}"
  ].join("\n");
}

function postScript(subreddit, id, maxComments) {
  return [
    "async () => {",
    "  const SUB = " + JSON.stringify(subreddit) + ";",
    "  const ID = " + JSON.stringify(String(id)) + ";",
    "  const MAXC = " + JSON.stringify(maxComments) + ";",
    "  const PERMALINK = '/r/' + SUB + '/comments/' + ID + '/';",
    FETCH_HELPER,
    PARA_HELPER,
    "  const res = await fetchHtml(PERMALINK + '?limit=' + MAXC + '&sort=top', 3);",
    "  if (res.loginWall) return { error: 'login_wall', id: ID };",
    "  if (res.fetchError) return { error: res.fetchError, id: ID };",
    "  const doc = new DOMParser().parseFromString(res.html, 'text/html');",
    "  const postEl = doc.querySelector('#siteTable .thing[data-fullname]') || doc.querySelector('.thing[data-fullname]');",
    "  if (!postEl) return { error: 'post not found', id: ID };",
    "  const titleEl = postEl.querySelector('a.title');",
    "  const title = titleEl ? (titleEl.textContent || '').trim() : '';",
    "  const author = postEl.getAttribute('data-author') || null;",
    "  const flairEl = postEl.querySelector('.linkflairlabel');",
    "  const flair = flairEl ? (flairEl.textContent || '').trim() : null;",
    "  const timeEl = postEl.querySelector('time');",
    "  const date = timeEl ? timeEl.getAttribute('datetime') : null;",
    "  const isSelf = postEl.getAttribute('data-is-self') === 'true' || postEl.classList.contains('self');",
    "  const outboundUrl = (titleEl && !isSelf) ? titleEl.getAttribute('href') : null;",
    "  const bodyEl = postEl.querySelector('.usertext-body .md');",
    "  const paragraphs = htmlToParas(bodyEl);",
    "  if (outboundUrl) paragraphs.unshift(outboundUrl);",
    "  const thumbImg = postEl.querySelector('.thumbnail img, a.thumbnail img');",
    "  const iconSrc = thumbImg ? thumbImg.getAttribute('src') : null;",
    "  const icon = iconSrc ? new URL(iconSrc, location.origin).href : null;",
    "  const commentCountEl = postEl.querySelector('.comments');",
    "  const commentCountMatch = (commentCountEl ? commentCountEl.textContent || '' : '').match(/(\\d[\\d,]*)/);",
    "  const commentCount = commentCountMatch ? parseInt(commentCountMatch[1].replace(/,/g, ''), 10) : 0;",
    // Budget is shared across the whole recursive tree, not per level: once
    // it hits zero every remaining call (siblings and descendants alike)
    // returns null and is filtered out.
    "  let remaining = MAXC;",
    "  const shapeComment = el => {",
    "    if (remaining <= 0 || !el.classList || !el.classList.contains('comment')) return null;",
    "    const commentAuthor = el.getAttribute('data-author') || null;",
    "    const entry = el.querySelector(':scope > .entry') || el.querySelector('.entry');",
    "    const body = entry ? entry.querySelector('.usertext-body .md') : null;",
    "    const timeEl2 = entry ? entry.querySelector('time') : null;",
    "    const deleted = commentAuthor === '[deleted]' || (body && /^\\s*\\[deleted\\]\\s*$/i.test(body.textContent || ''));",
    "    const text = body ? htmlToParas(body).join('\\n\\n') : '';",
    "    remaining -= 1;",
    "    const node = {",
    "      author: (commentAuthor && commentAuthor !== '[deleted]') ? commentAuthor : null,",
    "      icon: null,",
    "      text: text || (deleted ? '(deleted)' : ''),",
    "      date: timeEl2 ? timeEl2.getAttribute('datetime') : null",
    "    };",
    "    const childWrap = el.querySelector(':scope > .child > .sitetable') || el.querySelector('.child > .sitetable');",
    "    if (childWrap && remaining > 0) {",
    "      const replies = Array.from(childWrap.children).map(shapeComment).filter(Boolean);",
    "      if (replies.length) node.replies = replies;",
    "    }",
    "    return node;",
    "  };",
    "  const commentArea = doc.querySelector('.commentarea .sitetable.nestedlisting') || doc.querySelector('.commentarea > .sitetable');",
    "  const topLevel = commentArea ? Array.from(commentArea.children) : [];",
    "  const comments = [];",
    "  for (const el of topLevel) {",
    "    if (remaining <= 0) break;",
    "    const c = shapeComment(el);",
    "    if (c) comments.push(c);",
    "  }",
    "  return {",
    "    id: ID, subreddit: SUB, permalink: PERMALINK,",
    "    url: 'https://www.reddit.com' + PERMALINK,",
    "    title: title, author: author, date: date, flair: flair, icon: icon,",
    "    commentCount: commentCount,",
    "    contents: {",
    "      content: { type: 'doc', content: paragraphs.map(p => ({ type: 'paragraph', content: [{ type: 'text', text: p }] })) },",
    "      text: paragraphs.join('\\n\\n'),",
    "      paragraphs: paragraphs,",
    "      comments: comments",
    "    }",
    "  };",
    "}"
  ].join("\n");
}

// ── document mapping ───────────────────────────────────────────────────────

function slugify(s) {
  return (s || "").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 60);
}

function defaultPathForSearch(query, subreddit) {
  if (subreddit) return "/blogs/reddit/" + subreddit;
  return "/blogs/reddit/search/" + (slugify(query) || "query");
}

function defaultPathForPost(subreddit) {
  return "/blogs/reddit/" + subreddit;
}

// A post's own permalink, plus the subreddit and id, given either a url or
// an explicit subreddit+id pair. No `URL` API host-side (this runs in the
// wasm component, not the browser tab), so this is a plain regex match
// against `/r/<subreddit>/comments/<id>` — tolerant of a full URL, a bare
// path, or a trailing slug.
function resolvePost(input) {
  if (input.url) {
    const m = String(input.url).match(/\/r\/([^/]+)\/comments\/([a-z0-9]+)/i);
    if (!m) throw new Error("could not find /r/<subreddit>/comments/<id> in url: " + input.url);
    return { subreddit: m[1], id: m[2] };
  }
  if (input.subreddit && input.id) {
    return { subreddit: input.subreddit, id: String(input.id) };
  }
  throw new Error("either url, or subreddit and id, is required");
}

function countComments(list) {
  let n = 0;
  for (const c of list || []) {
    n += 1;
    n += countComments(c.replies);
  }
  return n;
}

const ICON_EXT_BY_CONTENT_TYPE = {
  "image/jpeg": "jpg",
  "image/png": "png",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp"
};

// Download the post's thumbnail (a hotlinked URL scraped from the page) and
// land it in the files store, returning the relPath `contents.icon` expects.
// Best-effort: a failed download must never fail the whole post.
function storeIcon(subreddit, id, iconUrl) {
  if (!iconUrl) return null;
  try {
    const resp = callExec("/builtin/web/http_request", { url: iconUrl, timeout_secs: 30 });
    if (!resp || resp.status < 200 || resp.status >= 300) {
      log("saveEntry: icon fetch failed for " + id + " (" + iconUrl + "): status=" + (resp && resp.status));
      return null;
    }
    const contentType = (resp.content_type || "").split(";")[0].trim().toLowerCase();
    const ext = ICON_EXT_BY_CONTENT_TYPE[contentType] || "jpg";
    const relPath = "files/docs/shared/reddit-" + subreddit + "-" + id + "-icon." + ext;
    callExec("/builtin/file/file_put", { rel_path: relPath, content: resp.body, encoding: resp.body_encoding });
    return relPath;
  } catch (e) {
    log("saveEntry: icon download failed for " + id + ": " + String(e).slice(0, 200));
    return null;
  }
}

function saveEntry(entry, path) {
  const slug = slugify(entry.title);
  const name = slug ? entry.id + "-" + slug : String(entry.id);
  const links = [
    { kind: "url", target: entry.url, title: entry.title || entry.url },
    { kind: "url", target: "https://www.reddit.com/r/" + entry.subreddit + "/", field: "subreddit", title: "r/" + entry.subreddit }
  ];
  if (entry.flair) {
    links.push({
      kind: "url",
      target: "https://www.reddit.com/r/" + entry.subreddit + "/",
      field: "tags",
      title: entry.flair
    });
  }
  const paras = entry.contents.paragraphs || [];
  const iconRelPath = storeIcon(entry.subreddit, entry.id, entry.icon);
  if (iconRelPath) entry.contents.icon = iconRelPath;
  callExec("/builtin/document/entity_save_document", {
    path: path,
    name: name,
    title: entry.title || "(untitled)",
    summary: (paras[0] || "").slice(0, 300),
    typeRef: TYPE_REF,
    contents: entry.contents,
    author: entry.author,
    pubDate: entry.date,
    links: links
  });
  return {
    id: entry.id,
    name: name,
    title: entry.title,
    subreddit: entry.subreddit,
    comments: countComments(entry.contents.comments),
    paragraphs: paras.length
  };
}

// ── actions ────────────────────────────────────────────────────────────────

function fetchPost(subreddit, id, maxComments) {
  const entry = evaluateInPage(postScript(subreddit, id, maxComments));
  if (entry.error === "login_wall") throw loginWallError();
  if (entry.error) throw new Error("post " + id + ": " + entry.error);
  return entry;
}

function extractPost(input) {
  const ref = resolvePost(input);
  const maxComments = input.maxComments || 20;
  const path = input.path || defaultPathForPost(ref.subreddit);
  ensureFirefox();
  log("extract_post: r/" + ref.subreddit + " id=" + ref.id);
  const entry = fetchPost(ref.subreddit, ref.id, maxComments);
  const result = saveEntry(entry, path);
  log("extract_post: saved " + result.name + " (comments=" + result.comments + "/" + entry.commentCount + ")");
  return result;
}

function searchPosts(input) {
  if (!input.query) throw new Error("query is required");
  const sort = input.sort || "relevance";
  const time = input.time || "all";
  const limit = input.limit || 10;
  const maxComments = input.maxComments || 20;
  const path = input.path || defaultPathForSearch(input.query, input.subreddit);
  ensureFirefox();
  log(
    "search_posts: query=" + input.query +
    (input.subreddit ? " subreddit=" + input.subreddit : "") +
    " limit=" + limit
  );
  const listing = evaluateInPage(searchScript(input.query, input.subreddit, sort, time, input.after));
  if (listing.error === "login_wall") throw loginWallError();
  if (listing.error) throw new Error("search: " + listing.error);
  log("search_posts: found " + listing.posts.length + " result(s)");

  const saved = [], failed = [];
  for (const item of listing.posts.slice(0, limit)) {
    if (isCancelled()) {
      log("search_posts: cancelled after " + saved.length + " saved");
      break;
    }
    try {
      const entry = fetchPost(item.subreddit, item.id, maxComments);
      saved.push(saveEntry(entry, path));
    } catch (e) {
      const msg = String(e).slice(0, 200);
      log("search_posts: post " + item.id + " failed: " + msg);
      failed.push({ id: item.id, error: msg });
    }
  }
  log("search_posts: done — saved=" + saved.length + " failed=" + failed.length);
  return {
    query: input.query,
    subreddit: input.subreddit || null,
    after: listing.after,
    path: path,
    saved: saved,
    failed: failed
  };
}

// componentize-qjs binds the WIT `runner` interface to a named export matching
// its identifier — a bare top-level `run` fails at runtime.
export const runner = {
  run(actionName, params) {
    let input;
    try {
      input = JSON.parse(params || "{}");
    } catch (e) {
      return { success: false, message: "invalid params JSON: " + e, output: null };
    }
    try {
      let out;
      switch (actionName) {
        case "extract_post": out = extractPost(input); break;
        case "search_posts": out = searchPosts(input); break;
        default:
          return { success: false, message: "unknown fn_name: " + actionName, output: null };
      }
      return { success: true, message: null, output: JSON.stringify(out) };
    } catch (e) {
      return { success: false, message: String((e && e.message) || e), output: null };
    }
  }
};

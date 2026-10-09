// solx-reddit — extract Reddit posts into BlogPostWithComments documents, and
// browse Reddit read-only.
//
// One wasm component backing several actions, dispatched on `fn_name`:
//   extract_post   — one post -> one document
//   search_posts   — a search (optionally scoped to one subreddit) -> N documents
//   list_posts     — a subreddit / front-page listing -> post summaries (no save)
//   get_post       — one post + comment tree returned inline (no save)
//   get_user       — a user's profile and overview/posts/comments/saved listing
//   subreddit_info — a subreddit's sidebar: subscribers, description, links
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
  // A missing subreddit redirects to subreddit search; an over-18 community
  // redirects to an interstitial that needs a one-time click-through.
  "    if ((r.url || '').indexOf('/over18') !== -1) return { fetchError: 'over-18 gate: open this community once in the managed Firefox profile and accept the prompt' };",
  "    if ((r.url || '').indexOf('/subreddits/search') !== -1 && url.indexOf('/subreddits/search') === -1) return { fetchError: 'subreddit not found' };",
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

const THING_NUM_HELPER = [
  "const toInt = s => { const m = String(s == null ? '' : s).replace(/,/g, '').match(/-?\\d+/); return m ? parseInt(m[0], 10) : null; };"
].join("\n");

// Parses old.reddit listing rows (`.thing`) — posts (t3_) and comments (t1_) —
// into flat summary objects, and finds the next-page cursor. Shared by
// reddit-list-posts and reddit-user so both return the same item shapes.
const THING_HELPER = [
  THING_NUM_HELPER,
  "const absUrl = (s) => { try { return new URL(s, 'https://old.reddit.com').href; } catch (e) { return null; } };",
  // Outbound links to Reddit itself point at old.reddit.com; hand back the canonical host.
  "const canon = (s) => { const u = absUrl(s); return u ? u.replace(/^https?:\\/\\/(?:old|np)\\.reddit\\.com\\//i, 'https://www.reddit.com/') : null; };",
  "const hostOf = (s) => { try { return new URL(s).hostname.replace(/^www\\./, ''); } catch (e) { return null; } };",
  "const parseThing = el => {",
  "  const fullname = el.getAttribute('data-fullname') || '';",
  "  const kind = el.classList.contains('comment') ? 'comment' : 'post';",
  "  const timeEl = el.querySelector('time');",
  "  const ts = parseInt(el.getAttribute('data-timestamp') || '', 10);",
  "  const created = (timeEl && timeEl.getAttribute('datetime')) || (ts ? new Date(ts).toISOString() : null);",
  "  const permalink = el.getAttribute('data-permalink') || null;",
  "  const item = {",
  "    kind: kind,",
  "    id: fullname.replace(/^t[0-9]_/, ''),",
  "    fullname: fullname,",
  "    subreddit: el.getAttribute('data-subreddit') || null,",
  "    author: el.getAttribute('data-author') || null,",
  "    created: created,",
  "    permalink: permalink",
  "  };",
  "  if (kind === 'post') {",
  "    const a = el.querySelector('a.title');",
  "    const isSelf = el.getAttribute('data-is-self') === 'true' || el.classList.contains('self');",
  "    const flairEl = el.querySelector('.linkflairlabel');",
  "    const thumb = el.querySelector('a.thumbnail img');",
  "    item.title = a ? (a.textContent || '').trim() : '';",
  "    item.score = toInt(el.getAttribute('data-score'));",
  "    item.commentCount = toInt(el.getAttribute('data-comments-count'));",
  "    item.flair = flairEl ? (flairEl.textContent || '').trim() : null;",
  "    item.isSelf = isSelf;",
  "    item.url = isSelf ? null : (a ? canon(a.getAttribute('href')) : null);",
  "    item.domain = el.getAttribute('data-domain') || (item.url ? hostOf(item.url) : null);",
  "    item.nsfw = el.getAttribute('data-nsfw') === 'true';",
  "    item.stickied = el.classList.contains('stickied');",
  "    item.thumbnail = thumb ? absUrl(thumb.getAttribute('src')) : null;",
  "  } else {",
  "    const parentTitle = el.querySelector('.parent a.title');",
  "    const bylink = el.querySelector('a.bylink');",
  "    const scoreEl = el.querySelector('.entry .tagline .score.unvoted');",
  "    const body = el.querySelector('.usertext-body .md');",
  "    if (!item.permalink && bylink) { try { item.permalink = new URL(bylink.getAttribute('href'), 'https://old.reddit.com').pathname; } catch (e) {} }",
  "    const pm = (item.permalink || '').match(/\\/comments\\/([a-z0-9]+)/i);",
  "    item.postId = pm ? pm[1] : null;",
  "    item.postTitle = parentTitle ? (parentTitle.textContent || '').trim() : null;",
  "    item.score = scoreEl ? toInt(scoreEl.getAttribute('title') || scoreEl.textContent) : null;",
  "    item.text = htmlToParas(body).join('\\n\\n');",
  "  }",
  "  return item;",
  "};",
  "const scrapeListing = doc => {",
  "  const rows = Array.from(doc.querySelectorAll('#siteTable > .thing[data-fullname]'));",
  "  const items = rows.filter(el => el.getAttribute('data-promoted') !== 'true').map(parseThing);",
  "  let after = null;",
  "  const next = doc.querySelector('.nav-buttons .next-button a');",
  "  if (next) {",
  "    try { after = new URL(next.getAttribute('href'), 'https://old.reddit.com').searchParams.get('after'); } catch (e) {}",
  "    if (!after && items.length) after = items[items.length - 1].fullname;",
  "  }",
  "  return { items: items, after: after };",
  "};"
].join("\n");

// A listing page: subreddit/front-page posts, or a user's overview/posts/
// comments/saved. `profile` additionally scrapes the user titlebox.
function listingScript(path, query, profile) {
  return [
    "async () => {",
    "  const PATH = " + JSON.stringify(path) + ";",
    "  const QS = " + JSON.stringify(query) + ";",
    "  const PROFILE = " + JSON.stringify(!!profile) + ";",
    FETCH_HELPER,
    PARA_HELPER,
    THING_HELPER,
    "  const res = await fetchHtml(PATH + QS, 3);",
    "  if (res.loginWall) return { error: 'login_wall' };",
    "  if (res.fetchError) return { error: res.fetchError };",
    "  const doc = new DOMParser().parseFromString(res.html, 'text/html');",
    "  const listing = scrapeListing(doc);",
    "  if (!listing.items.length) {",
    "    const wall = doc.querySelector('.interstitial');",
    "    if (wall) return { error: ((wall.textContent || '').replace(/\\s+/g, ' ').trim() || 'access restricted').slice(0, 200) };",
    "  }",
    "  const out = { items: listing.items, after: listing.after };",
    "  if (PROFILE) {",
    "    const tb = doc.querySelector('.side .titlebox') || doc.querySelector('.titlebox');",
    "    const ageTime = tb ? tb.querySelector('.age time') : null;",
    "    const lk = tb ? tb.querySelector('.karma:not(.comment-karma)') : null;",
    "    const ck = tb ? tb.querySelector('.karma.comment-karma') : null;",
    "    out.profile = {",
    "      linkKarma: lk ? toInt(lk.textContent) : null,",
    "      commentKarma: ck ? toInt(ck.textContent) : null,",
    "      created: ageTime ? ageTime.getAttribute('datetime') : null",
    "    };",
    "  }",
    "  return out;",
    "}"
  ].join("\n");
}

function subredditInfoScript(subreddit) {
  return [
    "async () => {",
    "  const SUB = " + JSON.stringify(subreddit) + ";",
    FETCH_HELPER,
    PARA_HELPER,
    THING_NUM_HELPER,
    "  const res = await fetchHtml('/r/' + SUB + '/', 3);",
    "  if (res.loginWall) return { error: 'login_wall' };",
    "  if (res.fetchError) return { error: res.fetchError };",
    "  const doc = new DOMParser().parseFromString(res.html, 'text/html');",
    "  const side = doc.querySelector('.side');",
    "  if (!side) {",
    "    const wall = doc.querySelector('.interstitial');",
    "    return { error: wall ? ((wall.textContent || '').replace(/\\s+/g, ' ').trim() || 'access restricted').slice(0, 200) : 'sidebar not found' };",
    "  }",
    "  const nameEl = side.querySelector('.redditname a');",
    "  const subs = side.querySelector('.subscribers .number');",
    "  const online = side.querySelector('.users-online .number');",
    "  const ageTime = side.querySelector('.age time');",
    "  const md = side.querySelector('.usertext-body .md');",
    "  const links = [];",
    "  if (md) md.querySelectorAll('a[href]').forEach(a => {",
    "    const href = a.getAttribute('href') || '';",
    "    if (!href || href.charAt(0) === '#' || href.indexOf('javascript') === 0) return;",
    "    let abs; try { abs = new URL(href, 'https://www.reddit.com').href; } catch (e) { return; }",
    "    if (links.length < 50) links.push({ text: (a.textContent || '').trim(), href: abs });",
    "  });",
    "  const pageTitle = doc.querySelector('title');",
    "  return {",
    "    subreddit: nameEl ? (nameEl.textContent || '').trim() : SUB,",
    "    title: pageTitle ? (pageTitle.textContent || '').trim() : null,",
    "    subscribers: subs ? toInt(subs.textContent) : null,",
    "    online: online ? toInt(online.textContent) : null,",
    "    created: ageTime ? ageTime.getAttribute('datetime') : null,",
    "    description: htmlToParas(md).join('\\n\\n'),",
    "    links: links",
    "  };",
    "}"
  ].join("\n");
}

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

// opts: { sort, depth, commentId, rich }. `rich` adds ids, scores, permalinks
// and "load more" markers to each comment (used by reddit-get-post);
// extract_post leaves it off so the saved document's comment shape is unchanged.
function postScript(subreddit, id, maxComments, opts) {
  const o = opts || {};
  return [
    "async () => {",
    "  const SUB = " + JSON.stringify(subreddit) + ";",
    "  const ID = " + JSON.stringify(String(id)) + ";",
    "  const MAXC = " + JSON.stringify(maxComments) + ";",
    "  const SORT = " + JSON.stringify(o.sort || "top") + ";",
    "  const DEPTH = " + JSON.stringify(o.depth || null) + ";",
    "  const FOCUS = " + JSON.stringify(o.commentId || null) + ";",
    "  const RICH = " + JSON.stringify(!!o.rich) + ";",
    "  const PERMALINK = '/r/' + SUB + '/comments/' + ID + '/';",
    FETCH_HELPER,
    PARA_HELPER,
    THING_NUM_HELPER,
    "  const fetchPath = FOCUS ? (PERMALINK + '_/' + FOCUS + '/') : PERMALINK;",
    "  const res = await fetchHtml(fetchPath + '?limit=' + MAXC + '&sort=' + SORT + (DEPTH ? '&depth=' + DEPTH : ''), 3);",
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
    "  const shapeMore = el => {",
    "    const a = el.querySelector('a');",
    "    const href = a ? a.getAttribute('href') : null;",
    "    const onclick = a ? (a.getAttribute('onclick') || '') : '';",
    "    const m = onclick.match(/morechildren\\(this,\\s*'[^']*',\\s*'([^']*)'/);",
    "    return {",
    "      more: true,",
    "      count: toInt(el.textContent),",
    "      href: (href && href.charAt(0) !== '#' && href.indexOf('javascript') !== 0) ? new URL(href, location.origin).pathname : null,",
    "      ids: m && m[1] ? m[1].split(',') : null",
    "    };",
    "  };",
    "  const shapeComment = (el, depth) => {",
    "    if (RICH && el.classList && (el.classList.contains('morechildren') || el.classList.contains('morerecursion'))) return shapeMore(el);",
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
    "    if (RICH) {",
    "      const scoreEl = entry ? entry.querySelector('.tagline .score.unvoted') : null;",
    "      node.id = (el.getAttribute('data-fullname') || '').replace(/^t1_/, '') || null;",
    "      node.score = scoreEl ? toInt(scoreEl.getAttribute('title') || scoreEl.textContent) : null;",
    "      node.permalink = el.getAttribute('data-permalink') || null;",
    "      node.depth = depth;",
    "    }",
    "    const childWrap = el.querySelector(':scope > .child > .sitetable') || el.querySelector('.child > .sitetable');",
    "    if (childWrap) {",
    "      const replies = Array.from(childWrap.children).map(c => shapeComment(c, depth + 1)).filter(Boolean);",
    "      if (replies.length) node.replies = replies;",
    "    }",
    "    return node;",
    "  };",
    "  const commentArea = doc.querySelector('.commentarea .sitetable.nestedlisting') || doc.querySelector('.commentarea > .sitetable');",
    "  const topLevel = commentArea ? Array.from(commentArea.children) : [];",
    "  const comments = [];",
    "  for (const el of topLevel) {",
    "    const c = shapeComment(el, 0);",
    "    if (c) comments.push(c);",
    "  }",
    "  const scoreMain = postEl.querySelector('.midcol .score.unvoted');",
    "  return {",
    "    id: ID, subreddit: SUB, permalink: PERMALINK,",
    "    url: 'https://www.reddit.com' + PERMALINK,",
    "    title: title, author: author, date: date, flair: flair, icon: icon,",
    "    commentCount: commentCount,",
    "    commentsReturned: MAXC - remaining,",
    "    fullname: postEl.getAttribute('data-fullname') || null,",
    "    score: toInt(postEl.getAttribute('data-score')) !== null ? toInt(postEl.getAttribute('data-score')) : (scoreMain ? toInt(scoreMain.getAttribute('title') || scoreMain.textContent) : null),",
    "    isSelf: isSelf, linkUrl: outboundUrl,",
    "    domain: postEl.getAttribute('data-domain') || null,",
    "    nsfw: postEl.getAttribute('data-nsfw') === 'true',",
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
    const m = String(input.url).match(/\/r\/([^/]+)\/comments\/([a-z0-9]+)(?:\/[^/?#]*\/([a-z0-9]+))?/i);
    if (!m) throw new Error("could not find /r/<subreddit>/comments/<id> in url: " + input.url);
    return { subreddit: m[1], id: m[2], commentId: m[3] || null };
  }
  if (input.subreddit && input.id) {
    return { subreddit: input.subreddit, id: String(input.id), commentId: null };
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

// Fetch an image through the signed-in Firefox tab. Reddit's image CDNs answer
// plain server-side requests with 403 but serve a real browser; the bytes come
// back base64-encoded inside the page script's result.
function iconScript(url) {
  return [
    "async () => {",
    "  const URL_ = " + JSON.stringify(url) + ";",
    "  let r;",
    "  try { r = await fetch(URL_, { credentials: 'omit' }); }",
    "  catch (e) { return { error: String(e) }; }",
    "  if (!r.ok) return { error: 'HTTP ' + r.status };",
    "  const bytes = new Uint8Array(await r.arrayBuffer());",
    "  if (bytes.length > 2000000) return { error: 'image too large' };",
    "  let bin = '';",
    "  for (let i = 0; i < bytes.length; i += 8192) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 8192));",
    "  return { contentType: r.headers.get('content-type') || '', body: btoa(bin) };",
    "}"
  ].join("\n");
}

// Plain HTTP first (cheap); on any failure fall back to the browser tab.
function downloadIcon(iconUrl) {
  try {
    const resp = callExec("/builtin/web/http-request", { url: iconUrl, timeout_secs: 30 });
    if (resp && resp.status >= 200 && resp.status < 300) {
      return { contentType: resp.content_type, body: resp.body, encoding: resp.body_encoding };
    }
    log("icon: direct fetch status=" + (resp && resp.status) + " for " + iconUrl + ", retrying via browser");
  } catch (e) {
    log("icon: direct fetch failed for " + iconUrl + ": " + String(e).slice(0, 120) + ", retrying via browser");
  }
  const out = evaluateInPage(iconScript(iconUrl));
  if (out.error) throw new Error("browser fetch: " + out.error);
  return { contentType: out.contentType, body: out.body, encoding: "base64" };
}

// Download the post's thumbnail (a hotlinked URL scraped from the page) and
// land it in the files store, returning the relPath `contents.icon` expects.
// Best-effort: a failed download must never fail the whole post.
function storeIcon(subreddit, id, iconUrl) {
  if (!iconUrl) return null;
  try {
    const img = downloadIcon(iconUrl);
    const contentType = (img.contentType || "").split(";")[0].trim().toLowerCase();
    const ext = ICON_EXT_BY_CONTENT_TYPE[contentType] || "jpg";
    const relPath = "files/docs/shared/reddit-" + subreddit + "-" + id + "-icon." + ext;
    callExec("/builtin/file/file-put", { rel_path: relPath, content: img.body, encoding: img.encoding });
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
  callExec("/builtin/document/entity-save-document", {
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

function fetchPost(subreddit, id, maxComments, opts) {
  const entry = evaluateInPage(postScript(subreddit, id, maxComments, opts));
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

// ── browse actions (read-only, nothing is saved) ───────────────────────────

const LISTING_SORTS = ["hot", "new", "top", "rising", "controversial"];
const TIME_RANGES = ["hour", "day", "week", "month", "year", "all"];
const COMMENT_SORTS = ["confidence", "top", "new", "controversial", "old", "qa"];

function pick(value, allowed, fallback, what) {
  const v = value || fallback;
  if (allowed.indexOf(v) === -1) throw new Error(what + " must be one of: " + allowed.join(", "));
  return v;
}

function clampLimit(limit, fallback) {
  const n = parseInt(limit, 10);
  if (!n || n < 1) return fallback;
  return Math.min(n, 100);
}

// "r/dndnext", "/r/dndnext/", "dndnext", "a+b" -> bare name; empty -> null.
// Validated because the name is interpolated into a request path.
function cleanSubreddit(s) {
  if (!s) return null;
  const name = String(s).trim().replace(/^\/?r\//i, "").replace(/\/+$/, "");
  if (!name) return null;
  if (!/^[A-Za-z0-9_+]+$/.test(name)) throw new Error("invalid subreddit name: " + s);
  return name;
}

function cleanUsername(s) {
  if (!s) throw new Error("username is required");
  const name = String(s).trim().replace(/^\/?(?:u|user)\//i, "").replace(/\/+$/, "");
  if (!/^[A-Za-z0-9_-]{1,40}$/.test(name)) throw new Error("invalid username: " + s);
  return name;
}

function buildQuery(pairs) {
  const parts = [];
  for (const k of Object.keys(pairs)) {
    const v = pairs[k];
    if (v !== null && v !== undefined && v !== "") parts.push(k + "=" + encodeURIComponent(v));
  }
  return parts.length ? "?" + parts.join("&") : "";
}

function runPageScript(script, what) {
  const out = evaluateInPage(script);
  if (out.error === "login_wall") throw loginWallError();
  if (out.error) throw new Error(what + ": " + out.error);
  return out;
}

function listPosts(input) {
  const subreddit = cleanSubreddit(input.subreddit);
  const sort = pick(input.sort, LISTING_SORTS, "hot", "sort");
  const time = pick(input.time, TIME_RANGES, "day", "time");
  const limit = clampLimit(input.limit, 25);
  const timed = sort === "top" || sort === "controversial";
  const path = (subreddit ? "/r/" + subreddit + "/" : "/") + (sort === "hot" ? "" : sort + "/");
  const qs = buildQuery({ limit: limit, t: timed ? time : null, after: input.after });
  ensureFirefox();
  log("list_posts: " + (subreddit ? "r/" + subreddit : "front page") + " sort=" + sort + " limit=" + limit);
  const out = runPageScript(listingScript(path, qs, false), "list posts");
  return {
    subreddit: subreddit,
    sort: sort,
    time: timed ? time : null,
    count: out.items.length,
    after: out.after,
    posts: out.items
  };
}

function getPost(input) {
  const ref = resolvePost(input);
  const maxComments = input.maxComments === 0 ? 0 : (input.maxComments || 50);
  const commentId = input.commentId || ref.commentId;
  const opts = {
    sort: pick(input.sort, COMMENT_SORTS, "confidence", "sort"),
    depth: input.depth || null,
    commentId: commentId,
    rich: true
  };
  ensureFirefox();
  log("get_post: r/" + ref.subreddit + " id=" + ref.id + (commentId ? " comment=" + commentId : ""));
  const e = fetchPost(ref.subreddit, ref.id, maxComments, opts);
  return {
    id: e.id,
    fullname: e.fullname,
    subreddit: e.subreddit,
    title: e.title,
    author: e.author,
    created: e.date,
    score: e.score,
    flair: e.flair,
    nsfw: e.nsfw,
    domain: e.domain,
    isSelf: e.isSelf,
    linkUrl: e.linkUrl ? e.linkUrl.replace(/^https?:\/\/(?:old|np)\.reddit\.com\//i, "https://www.reddit.com/") : null,
    thumbnail: e.icon,
    permalink: e.permalink,
    url: e.url,
    text: e.contents.text,
    commentCount: e.commentCount,
    commentsReturned: e.commentsReturned,
    focusedComment: commentId,
    comments: e.contents.comments
  };
}

const USER_VIEWS = ["overview", "submitted", "comments", "saved", "upvoted", "downvoted", "hidden"];
const USER_SORTS = ["new", "hot", "top", "controversial"];

function getUser(input) {
  const username = cleanUsername(input.username);
  const view = pick(input.view, USER_VIEWS, "overview", "view");
  const sort = pick(input.sort, USER_SORTS, "new", "sort");
  const time = pick(input.time, TIME_RANGES, "all", "time");
  const limit = clampLimit(input.limit, 25);
  const timed = sort === "top" || sort === "controversial";
  const qs = buildQuery({ sort: sort, t: timed ? time : null, limit: limit, after: input.after });
  ensureFirefox();
  log("get_user: u/" + username + " view=" + view);
  const out = runPageScript(listingScript("/user/" + username + "/" + view + "/", qs, true), "user " + username);
  return {
    username: username,
    view: view,
    profile: out.profile,
    count: out.items.length,
    after: out.after,
    items: out.items
  };
}

function subredditInfo(input) {
  const subreddit = cleanSubreddit(input.subreddit);
  if (!subreddit) throw new Error("subreddit is required");
  ensureFirefox();
  log("subreddit_info: r/" + subreddit);
  return runPageScript(subredditInfoScript(subreddit), "subreddit " + subreddit);
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
        case "list_posts": out = listPosts(input); break;
        case "get_post": out = getPost(input); break;
        case "get_user": out = getUser(input); break;
        case "subreddit_info": out = subredditInfo(input); break;
        default:
          return { success: false, message: "unknown fn_name: " + actionName, output: null };
      }
      return { success: true, message: null, output: JSON.stringify(out) };
    } catch (e) {
      return { success: false, message: String((e && e.message) || e), output: null };
    }
  }
};

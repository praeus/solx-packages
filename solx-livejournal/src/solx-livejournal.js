// solx-livejournal — extract a LiveJournal into BlogPostWithComments documents.
//
// One wasm component backing three actions, dispatched on `fn_name`:
//   extract_entry  — one entry -> one document
//   harvest_page   — one index page -> N documents, returns the "previous" link
//   harvest        — resumable loop over index pages, cursor in the env store
//
// Everything runs through the logged-in Firefox session: the page scripts use
// same-origin `fetch(..., {credentials:'include'})`, so friends-locked and
// private entries and screened comments are visible exactly as they are to the
// signed-in user. A signed-out session silently yields only public entries.

import { exec } from "sol:actions/action-exec@0.1.0";
import { log } from "sol:actions/logger@0.1.0";

const FIREFOX_EVAL = "/packages/solx-mcp-actions/firefox/mcp-firefox-evaluate-script";
const FIREFOX_START = "/packages/solx-firefox/firefox-start";
const FIREFOX_NAVIGATE = "/packages/solx-mcp-actions/firefox/mcp-firefox-navigate-page";
const TYPE_REF = "/types/docs/BlogPostWithComments";

// The firefox-devtools-mcp BiDi transport hardcodes a 10s per-command timeout
// (dist/index.js: `setTimeout(... , 1e4)`) and ignores the `timeout` param, so
// each page script must stay well under it. That is why entries are fetched
// one per call instead of batched.
const EVAL_TRIES = 4;

// ── host helpers ───────────────────────────────────────────────────────────

function callExec(ref, params) {
  const r = exec(ref, JSON.stringify(params || {}));
  if (!r || !r.success) {
    throw new Error(ref + ": " + ((r && r.message) || "action failed"));
  }
  return r.output ? JSON.parse(r.output) : null;
}

// Ask solx-core whether `action_stop` has been requested for this invocation.
// `/builtin/action/cancelled` resolves the *calling* action's own invocation
// (there is no way to pass an arbitrary id), so this is always "did someone
// stop *me*". Fails closed to `false`: a missing builtin or a failed call
// must never spuriously abort real work.
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

// Launch (or reuse) the managed Firefox instance solx-mcp-actions'
// firefox-devtools-mcp connects to, then point it at the journal. Page
// scripts fetch with relative URLs (same-origin, to carry the session
// cookie), which only resolve once the tab has a real origin — freshly
// launched Firefox opens to about:blank, where a relative fetch throws
// "TypeError: Window.fetch: / is not a valid URL."
//
// Cached per component instance: `harvest` calls this once per index page
// (up to max_pages times) via the same instance, and re-navigating every
// cycle is visible as a page reload in the tab even though the fetches
// underneath are same-origin and don't need it. Once a cycle has confirmed
// the journal is loaded, later cycles for the same user skip both calls.
let firefoxReadyUser = null;

function ensureFirefox(user) {
  if (firefoxReadyUser === user) return;
  callExec(FIREFOX_START, {});
  callExec(FIREFOX_NAVIGATE, { url: "https://" + user + ".livejournal.com/" });
  firefoxReadyUser = user;
}

// A dropped BiDi connection fails every fetch inside one script run, but a
// fresh evaluate call recovers — so retry at this level, not inside the page.
function evaluateInPage(js, wantKey) {
  let last = null;
  for (let i = 0; i < EVAL_TRIES; i++) {
    try {
      const out = unwrapEval(callExec(FIREFOX_EVAL, { function: js, timeout: 30000 }));
      if (out && (!wantKey || Object.prototype.hasOwnProperty.call(out, wantKey))) {
        return out;
      }
      last = out ? JSON.stringify(out).slice(0, 300) : "empty result";
    } catch (e) {
      last = String(e).slice(0, 300);
    }
  }
  throw new Error("page script failed after " + EVAL_TRIES + " tries: " + last);
}

// ── page scripts (run inside the logged-in Firefox tab) ────────────────────

// Shared prelude: a retrying same-origin fetch that carries the session.
const FETCH_HELPER = [
  "const fetchText = async (url, tries) => {",
  "  let last;",
  "  for (let i = 0; i < (tries || 2); i++) {",
  "    try {",
  "      const r = await fetch(url, { credentials: 'include' });",
  "      if (r.ok) return await r.text();",
  "      last = 'HTTP ' + r.status;",
  "    } catch (e) { last = String(e); }",
  "    await new Promise(s => setTimeout(s, 400));",
  "  }",
  "  throw new Error(last);",
  "};"
].join("\n");

function listIndexScript(user, index) {
  return [
    "async () => {",
    "  const USER = " + JSON.stringify(user) + ";",
    "  const INDEX = " + JSON.stringify(index) + ";",
    FETCH_HELPER,
    "  let html;",
    "  try { html = await fetchText(INDEX, 3); } catch (e) { return { error: String(e) }; }",
    "  const doc = new DOMParser().parseFromString(html, 'text/html');",
    // Entry containers carry the security icon; read id and security together.
    "  const seen = {}; const entries = [];",
    "  doc.querySelectorAll('.asset, .entry, .aentry').forEach(el => {",
    "    const ids = Array.from(el.querySelectorAll('a[href]'))",
    "      .map(a => (a.getAttribute('href') || '').match(/\\/(\\d+)\\.html/))",
    "      .filter(Boolean).map(m => m[1]);",
    "    if (!ids.length || seen[ids[0]]) return;",
    "    seen[ids[0]] = 1;",
    "    const icon = el.querySelector('.lj-entry-securityicon img, img[alt*=\"post]\"]');",
    "    const alt = icon ? (icon.getAttribute('alt') || '') : '';",
    "    entries.push({",
    "      id: ids[0],",
    "      security: /private/i.test(alt) ? 'private' : (/protected|friends/i.test(alt) ? 'friends' : 'public')",
    "    });",
    "  });",
    // Fall back to a flat link scan for themes without .asset containers.
    "  if (!entries.length) {",
    "    const re = new RegExp('^(?:https?://' + USER + '\\\\.livejournal\\\\.com)?/(\\\\d+)\\\\.html');",
    "    doc.querySelectorAll('a[href]').forEach(a => {",
    "      const h = a.getAttribute('href') || '';",
    "      const m = h.match(re);",
    "      if (m && h.indexOf('thread=') === -1 && !seen[m[1]]) {",
    "        seen[m[1]] = 1; entries.push({ id: m[1], security: 'unknown' });",
    "      }",
    "    });",
    "  }",
    "  let prev = null;",
    "  doc.querySelectorAll('a[href]').forEach(a => {",
    "    const h = a.getAttribute('href') || '';",
    "    if (!prev && h.indexOf('skip=') !== -1 && /previous/i.test(a.textContent || '')) {",
    "      const u = new URL(h, location.origin); prev = u.pathname + u.search;",
    "    }",
    "  });",
    "  return { index: INDEX, entries: entries, prev: prev };",
    "}"
  ].join("\n");
}

function extractEntryScript(user, id) {
  return [
    "async () => {",
    "  const USER = " + JSON.stringify(user) + ";",
    "  const ID = " + JSON.stringify(String(id)) + ";",
    FETCH_HELPER,
    "  let html;",
    "  try { html = await fetchText('/' + ID + '.html?format=light'); }",
    "  catch (e) { return { error: String(e), id: ID }; }",
    "  const doc = new DOMParser().parseFromString(html, 'text/html');",
    "  const q = s => doc.querySelector(s);",
    "  const og = p => { const m = doc.querySelector('meta[property=\"' + p + '\"]'); return m ? m.getAttribute('content') : null; };",
    // LiveJournal serves two template generations side by side in one journal:
    // older entries render .b-singlepost-*, newer ones .aentry-post__*.
    "  const bodyEl = q('.b-singlepost-body') || q('.aentry-post__text');",
    "  if (!bodyEl) return { error: 'no post body', id: ID };",
    "  bodyEl.querySelectorAll('script,style,.b-singlepost-tags,.aentry-post__tags,.lj-like,.b-adv').forEach(e => e.remove());",
    "  const title = ((q('.b-singlepost-title') || q('.aentry-post__title') || {}).textContent || og('og:title') || '').trim();",
    // Tag links are /tag/<name> in both templates.
    "  const tags = [];",
    "  doc.querySelectorAll('a[href*=\"/tag/\"]').forEach(a => {",
    "    const t = (a.textContent || '').trim();",
    "    if (t && tags.indexOf(t) === -1) tags.push(t);",
    "  });",
    "  const date = ((doc.querySelector('time') || {}).textContent || '').trim();",
    // The poster's userpic for this entry. Known header selectors are tried
    // first (both template generations); when neither matches, fall back to
    // any <img> outside the post body whose src looks like a userpic CDN
    // hotlink — scoped to exclude bodyEl so an image the author embedded in
    // the post text is never mistaken for their userpic.
    "  const pickIconUrl = () => {",
    "    const headerSelectors = ['.b-singlepost-author img', '.b-singlepost-icons img', '.aentry-post__author img', '.aentry-post__icon img', '.aentry-post__userpic img', '.ljuser img', '.i-ljuser-userpic', 'img.userpic'];",
    "    for (const sel of headerSelectors) {",
    "      const img = doc.querySelector(sel);",
    "      if (img && img.getAttribute('src')) return img.getAttribute('src');",
    "    }",
    "    const outside = Array.from(doc.querySelectorAll('img[src]')).filter(img => !bodyEl.contains(img));",
    "    const byPattern = outside.find(img => /userpic/i.test(img.getAttribute('src') || ''));",
    "    return byPattern ? byPattern.getAttribute('src') : null;",
    "  };",
    "  const iconSrc = pickIconUrl();",
    "  const icon = iconSrc ? new URL(iconSrc, location.origin).href : null;",
    "  const htmlToParas = el => {",
    "    const c = el.cloneNode(true);",
    "    c.querySelectorAll('br').forEach(b => b.replaceWith('\\n'));",
    "    c.querySelectorAll('p,div,blockquote,li,h1,h2,h3,h4').forEach(b => b.append('\\n'));",
    "    return (c.textContent || '').replace(/\\u00a0/g, ' ')",
    "      .split(/\\n\\s*\\n+/).map(s => s.replace(/[ \\t]+/g, ' ').trim()).filter(Boolean);",
    "  };",
    "  const paragraphs = htmlToParas(bodyEl);",
    // Comments are injected by Angular, so they are absent from the fetched
    // HTML. This internal JSON-RPC returns them with explicit parent pointers
    // and expand_all defeats thread collapsing.
    "  let flat = [], commentErr = null;",
    "  try {",
    "    const cr = await LJ.Api.callP('comment.get_thread', { journal: USER, itemid: +ID, expand_all: 1 });",
    "    flat = ((cr.result || cr).comments) || [];",
    "  } catch (e) { commentErr = JSON.stringify(e).slice(0, 200); }",
    "  const stripHtml = h => {",
    "    const d = document.createElement('div');",
    "    d.innerHTML = h || '';",
    "    d.querySelectorAll('br').forEach(b => b.replaceWith('\\n'));",
    "    return (d.textContent || '').replace(/\\u00a0/g, ' ').replace(/[ \\t]+/g, ' ').trim();",
    "  };",
    // comment.get_thread's exact userpic field isn't documented anywhere we
    // could find, so this checks every shape we've seen LJ-family JSON-RPCs
    // use elsewhere (bare url string, or an object carrying one) rather than
    // betting on a single name.
    "  const commentIconUrl = c => {",
    "    const pic = c.pic_url || c.userpic_url || c.pic || c.userpic;",
    "    if (typeof pic === 'string') return pic || null;",
    "    if (pic && typeof pic === 'object' && typeof pic.url === 'string') return pic.url || null;",
    "    return null;",
    "  };",
    "  const nodes = new Map();",
    "  flat.forEach(c => nodes.set(c.dtalkid, {",
    "    parent: c.parent || null,",
    "    author: c.dname || c.uname || null,",
    "    icon: commentIconUrl(c),",
    "    date: c.ctime || null,",
    "    subject: c.subject || '',",
    "    deleted: !!c.deleted,",
    "    text: stripHtml(c.article),",
    "    replies: []",
    "  }));",
    "  const roots = [];",
    "  nodes.forEach(n => {",
    "    const p = n.parent != null ? nodes.get(n.parent) : null;",
    "    if (p) p.replies.push(n); else roots.push(n);",
    "  });",
    "  const shape = n => {",
    "    const o = { author: n.author, icon: n.icon, text: n.text || (n.deleted ? '(deleted)' : ''), date: n.date };",
    "    if (n.subject) o.text = n.subject + '\\n\\n' + o.text;",
    "    if (n.replies.length) o.replies = n.replies.map(shape);",
    "    return o;",
    "  };",
    "  return {",
    "    id: ID,",
    "    url: 'https://' + USER + '.livejournal.com/' + ID + '.html',",
    "    title: title, author: USER, date: date, tags: tags, icon: icon,",
    "    commentCount: flat.length, commentErr: commentErr,",
    "    contents: {",
    "      content: { type: 'doc', content: paragraphs.map(p => ({ type: 'paragraph', content: [{ type: 'text', text: p }] })) },",
    "      text: paragraphs.join('\\n\\n'),",
    "      paragraphs: paragraphs,",
    "      comments: roots.map(shape)",
    "    }",
    "  };",
    "}"
  ].join("\n");
}

// ── document mapping ───────────────────────────────────────────────────────

function slugify(s) {
  return (s || "").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 60);
}

function defaultPath(user) {
  return "/blogs/livejournal/" + user;
}

const ICON_EXT_BY_CONTENT_TYPE = {
  "image/jpeg": "jpg",
  "image/png": "png",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp"
};

// Download the entry's userpic (a hotlinked URL scraped from the page) and
// land it in the files store, returning the relPath `contents.icon` expects.
// Unlike comment icons, the post-level icon can't stay a bare hotlink:
// solx-server's files route is bearer-token-gated, so the preview resolves
// it client-side from a relPath instead of loading a `src` URL directly.
// Best-effort: a failed download must never fail the whole entry.
function storeIcon(user, id, iconUrl) {
  if (!iconUrl) return null;
  try {
    const resp = callExec("/builtin/web/http_request", { url: iconUrl, timeout_secs: 30 });
    if (!resp || resp.status < 200 || resp.status >= 300) {
      log("saveEntry: icon fetch failed for " + id + " (" + iconUrl + "): status=" + (resp && resp.status));
      return null;
    }
    const contentType = (resp.content_type || "").split(";")[0].trim().toLowerCase();
    const ext = ICON_EXT_BY_CONTENT_TYPE[contentType] || "jpg";
    const relPath = "files/docs/shared/lj-" + user + "-" + id + "-icon." + ext;
    callExec("/builtin/file/file_put", { rel_path: relPath, content: resp.body, encoding: resp.body_encoding });
    return relPath;
  } catch (e) {
    log("saveEntry: icon download failed for " + id + ": " + String(e).slice(0, 200));
    return null;
  }
}

function saveEntry(entry, path, security) {
  const slug = slugify(entry.title);
  const name = slug ? entry.id + "-" + slug : String(entry.id);
  const links = [{ kind: "url", target: entry.url, title: entry.title || entry.url }];
  (entry.tags || []).forEach(t => {
    links.push({
      kind: "url",
      target: "https://" + entry.author + ".livejournal.com/tag/" + encodeURIComponent(t),
      field: "tags",
      title: t
    });
  });
  const paras = entry.contents.paragraphs || [];
  const iconRelPath = storeIcon(entry.author, entry.id, entry.icon);
  if (iconRelPath) entry.contents.icon = iconRelPath;
  callExec("/builtin/document/entity_save_document", {
    path: path,
    name: name,
    title: entry.title || "(untitled)",
    summary: (paras[0] || "").slice(0, 300),
    type_ref: TYPE_REF,
    contents: entry.contents,
    author: entry.author,
    pub_date: entry.date,
    links: links
  });
  return {
    id: entry.id,
    name: name,
    title: entry.title,
    security: security || null,
    comments: entry.commentCount,
    paragraphs: paras.length
  };
}

// ── actions ────────────────────────────────────────────────────────────────

function extractEntry(input) {
  if (!input.user) throw new Error("user is required");
  if (!input.id) throw new Error("id is required");
  const path = input.path || defaultPath(input.user);
  ensureFirefox(input.user);
  log("extract_entry: user=" + input.user + " id=" + input.id);
  const entry = evaluateInPage(extractEntryScript(input.user, input.id), "contents");
  if (entry.error) throw new Error("entry " + input.id + ": " + entry.error);
  const result = saveEntry(entry, path, input.security);
  log("extract_entry: saved " + result.name + " (comments=" + result.comments + ")");
  return result;
}

function harvestPage(input) {
  if (!input.user) throw new Error("user is required");
  const path = input.path || defaultPath(input.user);
  const index = input.index || "/";
  ensureFirefox(input.user);
  log("harvest_page: fetching index " + index);
  const listing = evaluateInPage(listIndexScript(input.user, index), "entries");
  if (listing.error) throw new Error("index " + index + ": " + listing.error);
  log("harvest_page: index " + index + " lists " + listing.entries.length + " entries");

  const saved = [], failed = [];
  for (const item of listing.entries) {
    try {
      const entry = evaluateInPage(extractEntryScript(input.user, item.id), "contents");
      if (entry.error) throw new Error(entry.error);
      saved.push(saveEntry(entry, path, item.security));
    } catch (e) {
      const msg = String(e).slice(0, 200);
      log("harvest_page: entry " + item.id + " failed: " + msg);
      failed.push({ id: item.id, error: msg });
    }
  }
  log("harvest_page: index " + index + " done — saved=" + saved.length + " failed=" + failed.length +
      (listing.prev ? " prev=" + listing.prev : " (last page)"));
  return { index: index, prev: listing.prev, saved: saved, failed: failed, path: path };
}

function harvest(input) {
  if (!input.user) throw new Error("user is required");
  // The cursor lives in its own namespace, one key per journal, and is written
  // with persist:true so it survives a solx-server restart rather than only
  // lasting the life of the process.
  const namespace = input.namespace || "livejournal";
  const cursorKey = input.cursor_key || input.user;
  const maxPages = input.max_pages || 1;

  let index = input.index;
  if (!index) {
    const got = callExec("/builtin/env/get_env", { namespace: namespace, key: cursorKey });
    index = (got && got.value) || "/";
  }
  log("harvest: user=" + input.user + " starting at cursor=" + index + " max_pages=" + maxPages);

  const pages = [];
  let totalSaved = 0, totalFailed = 0;
  for (let i = 0; i < maxPages; i++) {
    // Check for a stop request before pulling the next index page. The
    // cursor is advanced after every completed page, so stopping here loses
    // nothing: a resumed run picks up exactly where this one left off.
    if (isCancelled()) {
      log("harvest: cancelled after " + i + " page(s) — cursor=" + index);
      break;
    }
    log("harvest: page " + (i + 1) + "/" + maxPages + " — index=" + index);
    const page = harvestPage({ user: input.user, index: index, path: input.path });
    totalSaved += page.saved.length;
    totalFailed += page.failed.length;
    pages.push({
      index: page.index,
      prev: page.prev,
      saved: page.saved.length,
      failed: page.failed
    });
    // Advance the cursor after every page, not at the end: a run killed
    // mid-way then resumes at the first page it has not finished.
    if (page.prev) {
      callExec("/builtin/env/set_env", {
        namespace: namespace, key: cursorKey, value: page.prev, persist: true
      });
      index = page.prev;
    } else {
      pages[pages.length - 1].done = true;
      log("harvest: reached the last page (no 'previous' link) — stopping");
      break;
    }
  }
  log("harvest: finished — pages=" + pages.length + " total_saved=" + totalSaved +
      " total_failed=" + totalFailed + " cursor=" + index);
  return {
    user: input.user,
    namespace: namespace,
    cursor_key: cursorKey,
    cursor: index,
    pages: pages,
    total_saved: totalSaved,
    total_failed: totalFailed
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
        case "extract_entry": out = extractEntry(input); break;
        case "harvest_page": out = harvestPage(input); break;
        case "harvest": out = harvest(input); break;
        default:
          return { success: false, message: "unknown fn_name: " + actionName, output: null };
      }
      return { success: true, message: null, output: JSON.stringify(out) };
    } catch (e) {
      return { success: false, message: String((e && e.message) || e), output: null };
    }
  }
};

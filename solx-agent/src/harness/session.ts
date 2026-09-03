/**
 * The session document: reading it, writing it, and minting its name.
 *
 * One session is one conversation is one document. The widget holds no
 * transcript of its own, so a reload -- or another client, or `solx search
 * --path /agent/sessions` -- sees the same thread.
 *
 * The session type deliberately does **not** declare `messages` or `calls` as
 * properties. The full-text index walks only a type's declared fields, so
 * leaving them out keeps the whole transcript out of FTS while still storing
 * and validating it. That is what makes writing the document several times
 * per turn affordable -- see `turn.ts`.
 */

import { ADJECTIVES, NOUNS } from "./names";
import { GET_DOC, SAVE_DOC, SESSION_PATH, SESSION_TYPE } from "./refs";
import type { Host } from "./host";
import type { Session } from "./types";

export async function loadSession(host: Host, id: string): Promise<Session> {
  const doc = await host.call<{ contents?: Session }>(GET_DOC, { path: SESSION_PATH, name: id });
  if (!doc || !doc.contents) throw new Error("no session " + id);
  return doc.contents;
}

/**
 * `title` and `summary` are Document-level fields, so unlike the transcript
 * they *are* indexed -- which is the point. A list of sessions all titled
 * "Agent session <id>" says nothing about what any of them was for, and FTS
 * could not find one by what it was about.
 *
 * No timestamp is written here: `entity_save_document` already stamps the
 * document's own `updatedAt` on every write, which is what `solx list
 * document --sort-by created_at` reads. A second, locally-computed copy could
 * only drift from it.
 */
export function sessionTitle(id: string, session: Session): string {
  const title = session?.title || "";
  if (!title) return "Agent session " + id;
  return title.length > 80 ? title.slice(0, 79) + "…" : title;
}

export function sessionSummary(session: Session): string {
  const text = session?.answer || session?.title || "";
  return text.length > 500 ? text.slice(0, 499) + "…" : text;
}

export async function saveSession(host: Host, id: string, session: Session): Promise<Session> {
  await host.call(SAVE_DOC, {
    path: SESSION_PATH,
    name: id,
    title: sessionTitle(id, session),
    summary: sessionSummary(session),
    type_ref: SESSION_TYPE,
    contents: session,
  });
  return session;
}

/** How many word pairs to try before widening the namespace with a suffix. */
const NAME_ATTEMPTS = 4;

function pick(list: string[]): string {
  return list[Math.floor(Math.random() * list.length)];
}

function hex4(): string {
  return ("000" + Math.floor(Math.random() * 0x10000).toString(16)).slice(-4);
}

export function newMemoryName(): string {
  return "mem-" + pick(ADJECTIVES) + "-" + pick(NOUNS) + "-" + hex4();
}

/**
 * Does a session document already occupy this name?
 *
 * Fails **closed**: anything other than a definite not-found counts as taken.
 * A read that broke for some other reason must not be read as "free", because
 * the next thing that happens is a write.
 *
 * The `"not found"` match is a string coupling to solx-core's error text.
 * That is deliberate and load-bearing, so it is worth knowing it exists.
 */
async function sessionNameTaken(host: Host, name: string): Promise<boolean> {
  const r = await host.try(GET_DOC, { path: SESSION_PATH, name });
  if (r.ok) return true;
  return String(r.error || "").indexOf("not found") === -1;
}

/**
 * The existence check is not decoration. `entity_save_document` is an upsert
 * keyed on `(path, name)`, so a name collision does not fail -- it silently
 * writes over whatever was there, which for a live session means losing a
 * running transcript. 16,120 pairs is a birthday problem well within reach of
 * a busy install, so the check is what makes words safe here, not the size of
 * the lists.
 */
export async function newSessionId(host: Host): Promise<string> {
  for (let attempt = 0; attempt < NAME_ATTEMPTS; attempt++) {
    const name = pick(ADJECTIVES) + "-" + pick(NOUNS);
    if (!(await sessionNameTaken(host, name))) return name;
  }
  // Widen rather than keep drawing from a namespace that is evidently full:
  // still readable, and 65,536 times roomier.
  for (let attempt = 0; attempt < NAME_ATTEMPTS; attempt++) {
    const name = pick(ADJECTIVES) + "-" + pick(NOUNS) + "-" + hex4();
    if (!(await sessionNameTaken(host, name))) return name;
  }
  // Last resort, and never a failure. The "session-" prefix (never produced
  // by ADJECTIVES/NOUNS) marks it as the fallback tier at a glance.
  return "session-" + hex4() + hex4() + "-" + hex4();
}

/**
 * A hash of the tool name *and* its arguments, so an approval resolves to
 * exactly the call the operator was shown. Approving a bare tool name would
 * let a later iteration reuse that approval for different arguments -- the
 * same delete, a different document.
 */
export function callId(name: string, args: unknown): string {
  const s = name + " " + JSON.stringify(args || {});
  let h1 = 0x811c9dc5;
  let h2 = 0x01000193;
  for (let i = 0; i < s.length; i++) {
    h1 = (h1 ^ s.charCodeAt(i)) >>> 0;
    h1 = (h1 * 0x01000193) >>> 0;
    h2 = (h2 + s.charCodeAt(i) * (i + 7)) >>> 0;
  }
  return ("0000000" + h1.toString(16)).slice(-8) + ("0000000" + h2.toString(16)).slice(-8);
}

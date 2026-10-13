/**
 * The inquiry document: reading it, writing it, and minting its name.
 *
 * One inquiry is one conversation is one document, under `/inquiry/sessions`,
 * exactly as a session is for the agent -- so a reload, another client, or
 * `solx get doc` sees the same thread. The widget holds no transcript of its
 * own.
 *
 * Like the agent's session type, `InquirySession` does **not** declare the
 * bulky per-turn arrays (`turns`, and within a turn `documents`/`plan`) as
 * searchable properties. The full-text index walks only declared fields, so
 * leaving them out keeps the whole transcript -- and every gathered document
 * body -- out of FTS while still storing it. That is what makes writing the
 * document once per phase affordable.
 */

import { ADJECTIVES, NOUNS } from "./names";
import { GET_DOC, SAVE_DOC, SESSION_PATH, SESSION_TYPE } from "./refs";
import type { Host } from "./host";
import type { InquirySession } from "./types";

/**
 * Is this a state the pipeline is only ever in *while* it is running?
 *
 * Used by `loadInquiry` below. The pipeline is synchronous: a live status
 * exists only between a turn's first write and its last, inside one running
 * process. There is no `pending[]` resume the way the agent has, so a live
 * status read back from the document cannot be true -- whatever was running it
 * is gone.
 */
function isLive(status: InquirySession["status"]): boolean {
  return status === "planning" || status === "gathering" || status === "answering";
}

/**
 * How long a stored live status may go unwritten before it is presumed dead.
 *
 * The pipeline saves at every phase boundary, so the longest a *running* turn
 * goes without a write is one phase -- in practice the answer call. Fifteen
 * minutes matches the headless action's own `timeout_secs: 900`, past which
 * the host has killed the run anyway.
 */
export const STALE_LIVE_MS = 15 * 60 * 1000;

/** Milliseconds since a store timestamp, or `null` when it is absent or unreadable. */
function ageOf(updatedAt: unknown, now: number): number | null {
  if (typeof updatedAt !== "string") return null;
  // solx writes nanosecond fractions; trim to milliseconds before parsing.
  const t = Date.parse(updatedAt.replace(/(\.\d{3})\d+/, "$1"));
  return isFinite(t) ? now - t : null;
}

/**
 * Read an inquiry, closing out a turn that a killed process left live.
 *
 * The bug this exists for is real and was found by running the package: an
 * abandoned run (a cancelled MCP call, a stopped `solx exec`, a closed tab)
 * dies mid-`await` inside a phase, so the write-path `catch` never runs and the
 * document keeps saying `planning` forever. Nothing else will ever finish it --
 * `loadInquiry` has no resume to offer -- so the reader settles it here.
 *
 * The correction is written back, once, so the stored document stops lying
 * rather than only the in-memory copy. `turn_index` is not recomputed: the
 * turns that exist are what ran.
 *
 * Only a *stale* live status is closed out. A run that is still going in
 * another process -- a headless call over MCP, another tab -- looks exactly
 * like a dead one from here, and closing it would report a false
 * "interrupted" and write it into the document mid-run. Recent writes mean
 * something is still driving the turn; a document with no timestamp is
 * treated as stale, since nothing can vouch for it.
 */
export async function loadInquiry(
  host: Host,
  id: string,
  now: number = Date.now(),
): Promise<InquirySession> {
  const doc = await host.call<{ contents?: InquirySession; updatedAt?: string }>(GET_DOC, {
    path: SESSION_PATH,
    name: id,
  });
  if (!doc || !doc.contents) throw new Error("no inquiry " + id);
  const session = doc.contents;

  const age = ageOf(doc.updatedAt, now);
  if (isLive(session.status) && (age === null || age > STALE_LIVE_MS)) {
    session.status = "error";
    session.answer = null;
    const last = session.turns[session.turns.length - 1];
    if (last && isLive(last.status)) {
      last.status = "error";
      last.plan_error = last.plan_error ?? "interrupted";
    }
    // Best-effort: a read that can only be corrected by a write is still worth
    // correcting, but a failure to write must not fail the read.
    try {
      await saveInquiry(host, id, session);
    } catch {
      /* the in-memory correction is what the caller needs */
    }
  }

  return session;
}

/**
 * `title` and `summary` are Document-level fields, so unlike the transcript
 * they *are* indexed. A list of inquiries all titled "Inquiry <id>" would say
 * nothing about what any of them asked for, and FTS could not find one by its
 * question.
 */
export function inquiryTitle(id: string, session: InquirySession): string {
  const title = session?.title || "";
  if (!title) return "Inquiry " + id;
  return title.length > 80 ? title.slice(0, 79) + "…" : title;
}

export function inquirySummary(session: InquirySession): string {
  const text = session?.answer || session?.title || "";
  return text.length > 500 ? text.slice(0, 499) + "…" : text;
}

export async function saveInquiry(
  host: Host,
  id: string,
  session: InquirySession,
): Promise<InquirySession> {
  await host.call(SAVE_DOC, {
    path: SESSION_PATH,
    name: id,
    title: inquiryTitle(id, session),
    summary: inquirySummary(session),
    typeRef: SESSION_TYPE,
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

/**
 * Does an inquiry document already occupy this name?
 *
 * Fails **closed**: anything other than a definite not-found counts as taken.
 * A read that broke must not be read as "free", because the next thing that
 * happens is a write. The `"not found"` match is a string coupling to
 * solx-core's error text, and it is load-bearing.
 */
async function nameTaken(host: Host, name: string): Promise<boolean> {
  const r = await host.try(GET_DOC, { path: SESSION_PATH, name });
  if (r.ok) return true;
  return String(r.error || "").indexOf("not found") === -1;
}

/**
 * `entity-save-document` is an upsert keyed on `(path, name)`, so a collision
 * does not fail -- it silently overwrites, which for a live inquiry means
 * losing a running transcript. 16,120 pairs is a birthday problem well within
 * reach of a busy install, so the check is what makes words safe here, not the
 * size of the lists.
 */
export async function newInquiryId(host: Host): Promise<string> {
  for (let attempt = 0; attempt < NAME_ATTEMPTS; attempt++) {
    const name = pick(ADJECTIVES) + "-" + pick(NOUNS);
    if (!(await nameTaken(host, name))) return name;
  }
  for (let attempt = 0; attempt < NAME_ATTEMPTS; attempt++) {
    const name = pick(ADJECTIVES) + "-" + pick(NOUNS) + "-" + hex4();
    if (!(await nameTaken(host, name))) return name;
  }
  return "inquiry-" + hex4() + hex4() + "-" + hex4();
}

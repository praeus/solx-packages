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

export async function loadInquiry(host: Host, id: string): Promise<InquirySession> {
  const doc = await host.call<{ contents?: InquirySession }>(GET_DOC, {
    path: SESSION_PATH,
    name: id,
  });
  if (!doc || !doc.contents) throw new Error("no inquiry " + id);
  return doc.contents;
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

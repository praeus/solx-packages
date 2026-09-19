/**
 * Session naming and persistence.
 *
 * multi_inquire takes a `session` ref on every call and hands back an
 * updated `session_document` — but never saves it itself (see solx-inquiry's
 * session.rs: "multi_inquire performs no writes of its own anywhere"). A
 * fresh, never-persisted session ref is still a *valid* call (history just
 * reads back empty), which is what let the widget run before this module
 * existed — but it also means multi_inquire's intent phase never saw a
 * prior turn, so "what have we discussed?" always looked like a first
 * message. This module is what closes that loop: name a session with
 * solx-names, save the document `multi_inquire` returns after every turn,
 * and read it back — either to continue the current session across a
 * reload, or to switch to a different one from the picker.
 *
 * The call log under `src/console/` already keys its own document on
 * whatever `logId` the widget passes it. Passing the *same* name used here
 * is what gives the call log "the same root name as the session document"
 * — `/xprompt/sessions/<name>` and `/xprompt/call-logs/<name>` — without
 * this module needing to know anything about the console mechanism.
 *
 * Deletion is the one place that convention has to be known in one piece
 * rather than by coincidence: removing a session means removing both
 * documents, in an order that matters, so [`deleteSession`] below is the
 * single exception that reaches across to the call log's own path.
 */

import { compact } from "../../solx-widgets/src/wrap/host";
import type { Host } from "../../solx-widgets/src/wrap/host";
import { CALL_LOG_PATH } from "./console";
import { DELETE_DOC, GET_DOC, LIST_DOCS, RANDOM_NAME, SAVE_DOC, XPROMPT_SESSION_PATH } from "./refs";
import { normalizeResult } from "./result";
import type {
  StoredMultiInquireTurn,
  Turn,
  XPromptSessionDocument,
  XPromptSessionSummary,
} from "./types";

/** A fresh adjective-noun-id name, e.g. `capable-tiger-9f2c1a06`. `with_id` keeps two sessions from ever colliding. */
export async function randomSessionName(host: Host): Promise<string> {
  const result = await host.call<{ name: string }>(RANDOM_NAME, { with_id: true });
  return result.name;
}

/**
 * How many sessions the picker and the cleanup list can see.
 *
 * Raised from 50 once deleting became possible: a session past the limit is
 * invisible to *every* affordance the widget has, so the cap is not only how
 * much is listed but how much can ever be cleaned up. `total` comes back
 * alongside, so the UI can say when there are more rather than implying the
 * list is complete.
 */
export const SESSION_LIST_LIMIT = 200;

/** Existing sessions, most recently updated first — enough to label a picker's options. */
export async function listSessions(
  host: Host,
): Promise<{ sessions: XPromptSessionSummary[]; total: number }> {
  const r = await host.try<{ items?: Array<Record<string, unknown>>; total?: number }>(
    LIST_DOCS,
    compact({
      path_prefix: XPROMPT_SESSION_PATH,
      sort_by: "updatedAt",
      sort_order: "desc",
      limit: SESSION_LIST_LIMIT,
    }),
  );
  if (!r.ok) return { sessions: [], total: 0 };
  const sessions = (r.value?.items ?? [])
    .map((d) => ({
      name: String(d.name ?? ""),
      title: typeof d.title === "string" ? d.title : undefined,
      summary: typeof d.summary === "string" ? d.summary : undefined,
      updatedAt: typeof d.updatedAt === "string" ? d.updatedAt : undefined,
    }))
    .filter((s) => s.name);
  return {
    sessions,
    total: typeof r.value?.total === "number" ? r.value.total : sessions.length,
  };
}

/**
 * Delete a session: its call log, then the session document itself.
 *
 * **That order is the point.** A failure partway leaves the session still
 * listed and the operation retryable, and a call log with no session is inert
 * (`loadCallLog` fails open to empty). The other order strands a call log
 * whose session has vanished from the picker, with no affordance left that
 * could reach it.
 *
 * The call log is therefore best-effort — a session that never had one is the
 * normal case, and `entity-delete-document` on a missing document is not
 * something to fail a deletion over. The session document is not: if that
 * fails, nothing was deleted as far as the user is concerned, and this throws
 * so the caller can say so.
 *
 * What this **cannot** remove is the run's console output. Console entries are
 * keyed by `action_ref`, so every session's lines share one console with every
 * other session and every other caller of `multi-inquire`; `console/clear`
 * scopes to `before_seq`, not to an invocation. They age out on the console's
 * own ring buffer and TTL instead.
 */
export async function deleteSession(host: Host, name: string): Promise<void> {
  if (!name) return;
  await host.try(DELETE_DOC, { path: CALL_LOG_PATH, name });
  const r = await host.try<{ deleted?: boolean }>(DELETE_DOC, {
    path: XPROMPT_SESSION_PATH,
    name,
  });
  if (!r.ok) throw new Error(r.error);
}

/**
 * Read a session back as a transcript. A session that was never saved (a
 * brand-new name nobody has sent a turn under yet) reads back as an empty
 * transcript, not an error — the same "not found is just empty" reasoning
 * solx-inquiry's own `session::load` uses server-side.
 */
export async function loadSessionTranscript(host: Host, name: string): Promise<Turn[]> {
  const r = await host.try<{ contents?: { turns?: StoredMultiInquireTurn[] } }>(GET_DOC, {
    path: XPROMPT_SESSION_PATH,
    name,
  });
  if (!r.ok) return [];
  const turns = r.value?.contents?.turns ?? [];
  return turns.flatMap(storedTurnToEntries);
}

/**
 * Persist the `session_document` a turn's result carried. A complete
 * `entity-save-document` payload already — see solx-inquiry's
 * `session::build_document` — so this is a pass-through, not a transform.
 */
export async function saveSessionDocument(host: Host, doc: XPromptSessionDocument): Promise<void> {
  await host.call(SAVE_DOC, doc as unknown as Record<string, unknown>);
}

/**
 * One stored turn back into the `user` + `answer` pair the transcript
 * renders. `hits` is always empty and `model`/timestamps are always blank —
 * neither is part of what solx-inquiry stores in session history (see
 * `StoredMultiInquireTurn`), so a reloaded turn shows its text and mode but
 * not its suggested actions or cited documents, and sorts wherever its
 * position in `turns[]` (oldest first) puts it rather than by a real clock.
 */
function storedTurnToEntries(t: StoredMultiInquireTurn): Turn[] {
  // Through the same normaliser a live result goes through, rather than a
  // parallel set of `?? []` defaults: a stored turn is the least trustworthy
  // input the widget has (it was written by whichever build ran last), and
  // one definition of "safe to render" is easier to keep true than two.
  const result = normalizeResult({
    instruction: t.instruction,
    intent: { mode: t.mode, inquiries: t.inquiries, next_prompt: t.next_prompt },
    responses: t.responses,
    memories: t.memories,
    scripts: t.scripts,
    next_prompt: t.next_prompt,
    // Never stored - session history is orientation, not evidence, so
    // solx-inquiry keeps hits out of it.
    hits: [],
    notes: t.notes,
    errors: t.errors,
  });
  if (!result) return [];
  return [
    { kind: "user", text: result.instruction, at: "" },
    { kind: "answer", model: "", result, at: "" },
  ];
}

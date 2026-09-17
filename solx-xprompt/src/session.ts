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
 */

import { compact } from "../../solx-widgets/src/wrap/host";
import type { Host } from "../../solx-widgets/src/wrap/host";
import { GET_DOC, LIST_DOCS, RANDOM_NAME, SAVE_DOC, XPROMPT_SESSION_PATH } from "./refs";
import type {
  MultiInquireIntent,
  MultiInquireResult,
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

/** List existing sessions, most recently updated first — enough to label a picker's options. */
export async function listSessions(host: Host): Promise<XPromptSessionSummary[]> {
  const r = await host.try<{ items?: Array<Record<string, unknown>> }>(
    LIST_DOCS,
    compact({
      path_prefix: XPROMPT_SESSION_PATH,
      sort_by: "updatedAt",
      sort_order: "desc",
      limit: 50,
    }),
  );
  if (!r.ok) return [];
  return (r.value?.items ?? [])
    .map((d) => ({
      name: String(d.name ?? ""),
      title: typeof d.title === "string" ? d.title : undefined,
      summary: typeof d.summary === "string" ? d.summary : undefined,
      updatedAt: typeof d.updatedAt === "string" ? d.updatedAt : undefined,
    }))
    .filter((s) => s.name);
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
  const intent: MultiInquireIntent = {
    mode: t.mode,
    inquiries: t.inquiries ?? [],
    next_prompt: t.next_prompt ?? null,
  };
  const result: MultiInquireResult = {
    instruction: t.instruction,
    model: "",
    session: "",
    intent,
    responses: t.responses ?? [],
    memories: t.memories ?? [],
    scripts: t.scripts ?? [],
    next_prompt: t.next_prompt ?? null,
    hits: [],
    notes: t.notes ?? [],
    errors: t.errors ?? [],
  };
  return [
    { kind: "user", text: t.instruction, at: "" },
    { kind: "answer", model: "", result, at: "" },
  ];
}

import type { MergedEntry } from "../console";

/**
 * The merged console for this widget session's tracked calls — currently one
 * tracked call per turn, its `multi_inquire` invocation (see `dispatch.ts`) —
 * as one time-ordered feed. Reload-safe because the call log itself is a saved
 * document (see `src/console/`).
 *
 * Purely presentational: the tail loop lives in `useConsoleFeed`, owned by
 * `XPromptWidget`, so the feed keeps running while this panel is closed and
 * the same entries can drive the progress strip on the Chat tab.
 *
 * Note what does *not* appear here. `multi_inquire` drains each child model
 * call's console into its own, but `console-copy` stamps copied rows with the
 * source invocation id, and the client-side merge filters on the
 * `multi_inquire` invocation — so the streamed tokens are dropped and what
 * shows is the milestone prints. See `console/merge.ts`.
 */
export function ConsolePanel({
  entries,
  error,
}: {
  entries: MergedEntry[];
  error: string | null;
}) {
  return (
    <div className="col" style={{ gap: 6, flex: 1, minHeight: 120, overflow: "hidden" }}>
      {error && (
        <div className="chip danger" style={{ alignSelf: "flex-start" }}>
          {error}
        </div>
      )}
      <div
        className="col"
        style={{
          gap: 4,
          flex: 1,
          overflowY: "auto",
          background: "var(--bg-sunken)",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius)",
          padding: 8,
        }}
      >
        {entries.length === 0 && (
          <div className="faint" style={{ padding: 10 }}>
            No calls tracked yet. Send a prompt — each turn's recall, intent, and
            inquiry phases stream in here as they run.
          </div>
        )}
        {entries.map((e) => (
          <div
            key={e.invocationId + ":" + e.seq}
            className="row"
            style={{ gap: 6, alignItems: "baseline", flexWrap: "wrap" }}
          >
            <span className="faint" style={{ fontSize: 11 }}>{formatTime(e.ts)}</span>
            <span className="chip accent" style={{ fontSize: 10 }}>{e.label}</span>
            <span style={{ fontSize: 12, fontFamily: "var(--font-mono)", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
              {e.message ?? ""}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function formatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleTimeString();
  } catch {
    return iso;
  }
}

/**
 * Settings panel — renders inline as one of the three tabs (Chat / Console
 * / Settings). Holds the same state the popover used to (auto-loop toggle,
 * turn cap, approved-destructive allowlist, saved sessions) plus a "Clear
 * transcript" affordance that used to live on the toolbar.
 *
 * Mutates the parent's state; persists to localStorage via the parent
 * effects (see `LS_AUTO_LOOP` / `LS_AUTO_TURNS` in XPromptWidget.tsx).
 *
 * Lives inside a shadow root, so any cross-root listeners must use
 * `event.composedPath()` — there are none here because there's nothing
 * to dismiss: the tab strip is the affordance.
 */

import type { XPromptSessionSummary } from "../types";

export interface SettingsPanelProps {
  autoLoop: boolean;
  setAutoLoop: (v: boolean) => void;
  autoTurns: number;
  setAutoTurns: (v: number) => void;
  approvedCount: number;
  onClearApprovals: () => void;
  /** Most recently updated first, as `listSessions` returns them. */
  sessions: XPromptSessionSummary[];
  /** How many exist in total, which is not how many are listed. */
  sessionTotal: number;
  currentSession: string;
  onDeleteSession: (name: string) => void;
  /** Deleting mid-turn would pull a session out from under a running call. */
  busy: boolean;
  /** Clears the in-memory transcript (the localStorage copy is rewritten
   *  by the parent effect on the next change). */
  onClearTranscript: () => void;
}

export function SettingsPanel({
  autoLoop,
  setAutoLoop,
  autoTurns,
  setAutoTurns,
  approvedCount,
  onClearApprovals,
  sessions,
  sessionTotal,
  currentSession,
  onDeleteSession,
  busy,
  onClearTranscript,
}: SettingsPanelProps) {
  return (
    <div
      className="col"
      role="region"
      aria-label="Settings"
      style={{
        gap: 12,
        padding: 12,
        flex: 1,
        minHeight: 120,
        maxHeight: "60vh",
        overflowY: "auto",
        background: "var(--bg-sunken)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius)",
      }}
    >
      <section className="col" style={{ gap: 8 }}>
        <strong style={{ fontSize: 12 }}>Auto-run</strong>
        <label className="row" style={{ gap: 6, alignItems: "flex-start" }}>
          <input
            type="checkbox"
            checked={autoLoop}
            onChange={(e) => setAutoLoop(e.target.checked)}
            style={{ marginTop: 2 }}
          />
          <span style={{ fontSize: 12 }}>
            Auto-loop follow-ups
            <span className="faint" style={{ fontSize: 10, display: "block" }}>
              When a turn ends with <code>next_prompt</code>, run it as the next instruction.
            </span>
          </span>
        </label>
        <label className="row" style={{ gap: 6, alignItems: "center" }}>
          <span style={{ fontSize: 12, minWidth: 80 }}>Turn cap</span>
          <input
            type="number"
            min={1}
            max={10}
            value={autoTurns}
            // `setAutoTurns` clamps too — this is the affordance, not the
            // guard. See `clampAutoTurns`.
            onChange={(e) => {
              const n = parseInt(e.target.value, 10);
              if (Number.isFinite(n)) setAutoTurns(n);
            }}
            style={{ width: 60 }}
          />
          <span className="faint" style={{ fontSize: 10 }}>(1–10)</span>
        </label>
      </section>

      <section
        className="col"
        style={{ gap: 8, borderTop: "1px solid var(--border)", paddingTop: 10 }}
      >
        <strong style={{ fontSize: 12 }}>Destructive approvals</strong>
        <div
          className="row"
          style={{ gap: 6, alignItems: "center", justifyContent: "space-between" }}
        >
          <span className="faint" style={{ fontSize: 11 }}>
            {approvedCount} approved destructive action
            {approvedCount === 1 ? "" : "s"}
          </span>
          <button
            onClick={onClearApprovals}
            disabled={approvedCount === 0}
            style={{ fontSize: 11 }}
            title="Forget every destructive action approved in this session"
          >
            Clear approvals
          </button>
        </div>
      </section>

      <section
        className="col"
        style={{ gap: 8, borderTop: "1px solid var(--border)", paddingTop: 10 }}
      >
        <strong style={{ fontSize: 12 }}>Transcript</strong>
        <div className="row" style={{ gap: 6, alignItems: "center" }}>
          <button
            onClick={onClearTranscript}
            disabled={busy}
            title="Clear the in-memory transcript"
            style={{ fontSize: 11 }}
          >
            Clear transcript
          </button>
          <span className="faint" style={{ fontSize: 11 }}>
            The localStorage copy is rewritten on the next change.
          </span>
        </div>
      </section>

      {/* Cleanup for sessions other than the one you are on. The picker
          can only ever act on the current selection, so without this,
          binning five old sessions meant switching to each one first and
          loading its transcript on the way. */}
      <section
        className="col"
        style={{ gap: 8, borderTop: "1px solid var(--border)", paddingTop: 10 }}
      >
        <strong style={{ fontSize: 12 }}>Sessions</strong>
        {sessions.length === 0 && (
          <span className="faint" style={{ fontSize: 11 }}>
            No saved sessions yet — one is saved after its first turn.
          </span>
        )}
        <div className="col" style={{ gap: 2, maxHeight: 200, overflowY: "auto" }}>
          {sessions.map((s) => (
            <div
              key={s.name}
              className="row"
              style={{ gap: 6, alignItems: "baseline", justifyContent: "space-between" }}
            >
              <span
                style={{
                  fontSize: 11,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
                title={[s.name, s.summary].filter(Boolean).join("\n\n")}
              >
                {s.title || s.name}
                {s.name === currentSession && (
                  <span className="faint" style={{ fontSize: 10 }}> (current)</span>
                )}
              </span>
              <span className="row" style={{ gap: 4, alignItems: "baseline" }}>
                {s.updatedAt && (
                  <span className="faint" style={{ fontSize: 10, whiteSpace: "nowrap" }}>
                    {formatDay(s.updatedAt)}
                  </span>
                )}
                <button
                  onClick={() => onDeleteSession(s.name)}
                  disabled={busy}
                  title={`Delete "${s.name}" and its saved history`}
                  style={{ fontSize: 11, padding: "0 6px" }}
                >
                  ×
                </button>
              </span>
            </div>
          ))}
        </div>
        {sessionTotal > sessions.length && (
          // Saying so rather than implying the list is everything: a
          // session past the cap cannot be reached by any affordance here.
          <span className="faint" style={{ fontSize: 10 }}>
            showing {sessions.length} of {sessionTotal} — older sessions are not listed
          </span>
        )}
      </section>
    </div>
  );
}

/** A short, locale-formatted day for a session's `updatedAt`. */
function formatDay(iso: string): string {
  try {
    return new Date(iso).toLocaleDateString();
  } catch {
    return "";
  }
}

import { useEffect, useState } from "react";
import type { OllamaModel, SessionStatus } from "../harness";

/** How each status reads to someone watching. */
const STATUS_LABEL: Record<SessionStatus, { text: string; cls: string }> = {
  running: { text: "working", cls: "chip accent" },
  awaiting_approval: { text: "needs approval", cls: "chip warn" },
  // Not "done": the model yields the turn to answer *and* to ask, and the
  // next message reopens either. Terminal framing here would be a lie about
  // what happened -- which is why the status itself is now called `idle`.
  idle: { text: "your turn", cls: "chip ok" },
  blocked: { text: "blocked", cls: "chip danger" },
  exhausted: { text: "out of iterations", cls: "chip warn" },
  cancelled: { text: "stopped", cls: "chip" },
};

export interface SessionSummary {
  name: string;
  title?: string;
  summary?: string;
}

export function Header({
  models,
  model,
  onModel,
  status,
  iteration,
  maxIterations,
  sessionId,
  title,
  sessions,
  onOpenSession,
  onNewSession,
  onDeleteSession,
  busy,
}: {
  models: OllamaModel[];
  model: string;
  onModel: (model: string) => void;
  status: SessionStatus | null;
  iteration: number;
  maxIterations: number;
  sessionId: string | null;
  /** Human-readable session title; the raw id is the fallback. */
  title: string | null;
  sessions: SessionSummary[];
  onOpenSession: (id: string) => void;
  onNewSession: () => void;
  onDeleteSession: () => void;
  busy: boolean;
}) {
  const [confirmDelete, setConfirmDelete] = useState(false);
  const badge = status ? STATUS_LABEL[status] : null;

  // Disarm whenever the session changes, by any route -- the select below,
  // the New button, or the widget switching sessions on its own. The confirm
  // pair is hidden while `sessionId` is null rather than unmounted-and-reset,
  // so without this an armed Delete from a previous session comes back
  // already armed over the next one, and a single click deletes the wrong
  // thread.
  useEffect(() => setConfirmDelete(false), [sessionId]);

  return (
    <div className="col" style={{ gap: 5 }}>
      <div className="row" style={{ gap: 6, justifyContent: "space-between" }}>
        {models.length > 0 ? (
          <select
            value={model}
            onChange={(e) => onModel(e.target.value)}
            // The transcript records which model produced which turn, so
            // switching mid-session would misdescribe what is already there.
            disabled={!!sessionId}
            style={{ flex: 1, minWidth: 0 }}
          >
            {!model && <option value="">Pick a model…</option>}
            {models.map((m) => (
              <option key={m.name} value={m.name}>
                {m.name}
              </option>
            ))}
          </select>
        ) : (
          <input
            placeholder="model name (e.g. qwen3:4b)"
            value={model}
            disabled={!!sessionId}
            onChange={(e) => onModel(e.target.value)}
            style={{ flex: 1, minWidth: 0 }}
          />
        )}

        <select
          value={sessionId ?? ""}
          onChange={(e) => {
            const id = e.target.value;
            setConfirmDelete(false);
            if (id) onOpenSession(id);
            else onNewSession();
          }}
          disabled={busy}
          title="Switch session"
          style={{ flex: 1, minWidth: 0 }}
        >
          <option value="">— New session —</option>
          {sessions.map((s) => (
            <option key={s.name} value={s.name}>
              {s.title || s.name}
            </option>
          ))}
        </select>

        {sessionId &&
          (confirmDelete ? (
            <span className="row" style={{ gap: 4 }}>
              <button onClick={() => { setConfirmDelete(false); onDeleteSession(); }} disabled={busy}>
                Confirm
              </button>
              <button onClick={() => setConfirmDelete(false)} disabled={busy}>
                Keep
              </button>
            </span>
          ) : (
            <button
              onClick={() => setConfirmDelete(true)}
              disabled={busy}
              title="Delete this session"
            >
              Delete
            </button>
          ))}
      </div>

      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        {badge && <span className={badge.cls}>{badge.text}</span>}
        {sessionId && (
          <>
            <span className="chip">{title || sessionId}</span>
            <span className="faint" style={{ fontSize: 11 }}>
              iteration {iteration}/{maxIterations} this turn
            </span>
          </>
        )}
        {models.length === 0 && (
          <span className="faint" style={{ fontSize: 11 }}>
            solx-ollama not reachable — type a model name
          </span>
        )}
      </div>
    </div>
  );
}

import { useState } from "react";
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
  sessions,
  onOpenSession,
  onNewSession,
  busy,
}: {
  models: OllamaModel[];
  model: string;
  onModel: (model: string) => void;
  status: SessionStatus | null;
  iteration: number;
  maxIterations: number;
  sessionId: string | null;
  sessions: SessionSummary[];
  onOpenSession: (id: string) => void;
  onNewSession: () => void;
  busy: boolean;
}) {
  const [historyOpen, setHistoryOpen] = useState(false);
  const badge = status ? STATUS_LABEL[status] : null;

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
        <button onClick={() => setHistoryOpen((v) => !v)} disabled={busy}>
          History
        </button>
        <button onClick={onNewSession} disabled={busy}>
          New
        </button>
      </div>

      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        {badge && <span className={badge.cls}>{badge.text}</span>}
        {sessionId && (
          <>
            <span className="chip">{sessionId}</span>
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

      {historyOpen && (
        <div
          className="col"
          style={{
            gap: 3,
            border: "1px solid var(--border)",
            borderRadius: "var(--radius)",
            padding: 6,
            maxHeight: 180,
            overflowY: "auto",
            background: "var(--bg-raised)",
          }}
        >
          {sessions.length === 0 && <span className="faint" style={{ fontSize: 11 }}>No sessions yet.</span>}
          {sessions.map((s) => (
            <button
              key={s.name}
              onClick={() => {
                setHistoryOpen(false);
                onOpenSession(s.name);
              }}
              style={{ background: "none", border: "none", textAlign: "left", padding: "3px 0" }}
            >
              <span className="col" style={{ gap: 1 }}>
                <span style={{ fontSize: 12 }}>{s.title || s.name}</span>
                <span className="faint" style={{ fontSize: 11 }}>{s.name}</span>
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

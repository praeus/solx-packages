import { useState } from "react";
import type { RenderedCall } from "../transcript";

const OUTCOME_CLASS: Record<string, string> = {
  ok: "chip ok",
  error: "chip danger",
  refused: "chip danger",
  denied: "chip warn",
};

function pretty(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/**
 * One tool call: what was asked for, what it resolved to, and how it went.
 *
 * The arguments matter as much as the name -- the same tool with different
 * arguments is a different action, which is why approval is per call rather
 * than per tool -- so they are always available, one click away.
 */
export function ToolCallCard({ call }: { call: RenderedCall }) {
  const [open, setOpen] = useState(false);
  const outcome = call.outcome ?? (call.result === null ? "running" : null);

  return (
    <div
      style={{
        border: "1px solid var(--border)",
        borderRadius: "var(--radius)",
        background: "var(--bg-raised)",
        padding: "5px 7px",
      }}
    >
      <div className="row" style={{ justifyContent: "space-between", gap: 8 }}>
        <button
          onClick={() => setOpen((v) => !v)}
          style={{
            background: "none",
            border: "none",
            padding: 0,
            textAlign: "left",
            flex: 1,
            minWidth: 0,
            fontFamily: "var(--font-mono)",
            fontSize: 12,
          }}
          title={call.ref ?? call.name}
        >
          <span className="faint">{open ? "▾" : "▸"} </span>
          {call.name}
        </button>
        {outcome === "running" ? (
          <span className="chip">running…</span>
        ) : outcome ? (
          <span className={OUTCOME_CLASS[outcome] ?? "chip"}>{outcome}</span>
        ) : null}
        {call.approvedBy && <span className="chip warn">approved</span>}
      </div>

      {open && (
        <div className="col" style={{ gap: 4, marginTop: 5 }}>
          {call.ref && (
            <div className="faint" style={{ fontSize: 11, fontFamily: "var(--font-mono)" }}>
              {call.ref}
            </div>
          )}
          <div className="muted" style={{ fontSize: 11 }}>arguments</div>
          <pre>{pretty(call.arguments)}</pre>
          {call.result !== null && (
            <>
              <div className="muted" style={{ fontSize: 11 }}>result</div>
              <pre style={{ maxHeight: 200, overflowY: "auto" }}>{call.result}</pre>
            </>
          )}
        </div>
      )}
    </div>
  );
}

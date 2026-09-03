import { useState } from "react";
import type { PendingView } from "../harness";

function pretty(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/**
 * The destructive calls a turn stopped on, and the decision they need.
 *
 * Every argument is shown, unfolded, because this is the only place they are
 * ever surfaced and because approval is bound to them: a call_id covers one
 * exact set of arguments, so approving here cannot be reused later for the
 * same tool pointed at something else.
 *
 * Nothing is preselected. Anything left unchecked is denied when this is
 * submitted, which is the safe direction to fail.
 */
export function ApprovalCard({
  pending,
  busy,
  onDecide,
}: {
  pending: PendingView[];
  busy: boolean;
  onDecide: (approve: string[]) => void;
}) {
  const [checked, setChecked] = useState<Record<string, boolean>>({});
  const approved = pending.filter((p) => checked[p.call_id]).map((p) => p.call_id);

  return (
    <div
      className="col"
      style={{
        gap: 8,
        border: "1px solid color-mix(in srgb, var(--warn) 45%, var(--border))",
        background: "var(--warn-soft)",
        borderRadius: "var(--radius)",
        padding: 9,
      }}
    >
      <div className="row" style={{ gap: 6 }}>
        <strong style={{ color: "var(--warn)" }}>
          Approval needed — {pending.length} destructive {pending.length === 1 ? "call" : "calls"}
        </strong>
      </div>

      {pending.map((call) => (
        <label key={call.call_id} className="col" style={{ gap: 4, cursor: "pointer" }}>
          <span className="row" style={{ gap: 6, alignItems: "flex-start" }}>
            <input
              type="checkbox"
              checked={!!checked[call.call_id]}
              disabled={busy}
              onChange={(e) =>
                setChecked((prev) => ({ ...prev, [call.call_id]: e.target.checked }))
              }
              style={{ width: "auto", marginTop: 3 }}
            />
            <span className="col" style={{ gap: 2, minWidth: 0 }}>
              <span style={{ fontFamily: "var(--font-mono)", fontSize: 12 }}>{call.name}</span>
              {call.ref && (
                <span className="faint" style={{ fontSize: 11, fontFamily: "var(--font-mono)" }}>
                  {call.ref}
                </span>
              )}
            </span>
          </span>
          <pre style={{ maxHeight: 160, overflowY: "auto" }}>{pretty(call.arguments)}</pre>
        </label>
      ))}

      <div className="row" style={{ gap: 6, justifyContent: "flex-end" }}>
        <button disabled={busy} onClick={() => onDecide([])}>
          Deny all
        </button>
        <button
          className="primary"
          disabled={busy || approved.length === 0}
          onClick={() => onDecide(approved)}
        >
          Approve {approved.length > 0 ? `${approved.length} ` : ""}selected
        </button>
      </div>
      <span className="faint" style={{ fontSize: 11 }}>
        Anything left unchecked is denied.
      </span>
    </div>
  );
}

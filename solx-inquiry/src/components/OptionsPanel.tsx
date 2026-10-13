import { useState } from "react";
import type { SetupPrefs } from "../session/store";
import { MAX_DOCS_HARD, MAX_PLAN_STEPS_HARD, MAX_DOC_BUDGET } from "../harness";

/**
 * The tuning panel.
 *
 * Smaller than the agent's, because there is less to tune and nothing to
 * fence. There is no grant here and no memory scope -- every verb is a read,
 * and there is no cross-session state. What is left is exactly the three
 * budgets that decide how much an answer can draw on, plus two flags.
 *
 * These are *live*: unlike the agent, where a session's catalogue cap is fixed
 * at creation because the transcript records what was resolvable, inquiry's
 * budgets are read at the top of every turn and can change between questions.
 * So the panel never locks.
 */
export function OptionsPanel({
  setup,
  onChange,
  live,
}: {
  setup: SetupPrefs;
  onChange: (next: SetupPrefs) => void;
  /** True once an inquiry exists: shows that changes apply from the next question. */
  live: boolean;
}) {
  const [open, setOpen] = useState(false);

  return (
    <div
      className="col"
      style={{
        gap: 6,
        border: "1px solid var(--border)",
        borderRadius: "var(--radius)",
        padding: 7,
        background: "var(--bg-raised)",
      }}
    >
      <button
        onClick={() => setOpen((v) => !v)}
        style={{ background: "none", border: "none", padding: 0, textAlign: "left" }}
      >
        <span className="row" style={{ gap: 6 }}>
          <span className="muted">{open ? "▾" : "▸"} Options</span>
          {live && <span className="chip">applies from the next question</span>}
        </span>
      </button>

      {open && (
        <div className="col" style={{ gap: 8 }}>
          <div className="row" style={{ gap: 6 }}>
            <label className="col" style={{ gap: 3, flex: 1 }}>
              <span className="muted" style={{ fontSize: 11 }}>
                Max lookups per question
              </span>
              <input
                type="number"
                min={1}
                max={MAX_PLAN_STEPS_HARD}
                value={setup.maxPlanSteps}
                onChange={(e) =>
                  onChange({ ...setup, maxPlanSteps: Number(e.target.value) || 4 })
                }
              />
            </label>
            <label className="col" style={{ gap: 3, flex: 1 }}>
              <span className="muted" style={{ fontSize: 11 }}>
                Max documents per answer
              </span>
              <input
                type="number"
                min={1}
                max={MAX_DOCS_HARD}
                value={setup.maxDocs}
                onChange={(e) => onChange({ ...setup, maxDocs: Number(e.target.value) || 12 })}
              />
            </label>
          </div>

          <label className="col" style={{ gap: 3 }}>
            <span className="muted" style={{ fontSize: 11 }}>
              Text budget for the answer, in characters ({setup.docBudget} of {MAX_DOC_BUDGET}).
              Documents are shared across this, so a larger budget fits more of each.
            </span>
            <input
              type="range"
              min={2000}
              max={MAX_DOC_BUDGET}
              step={1000}
              value={setup.docBudget}
              onChange={(e) => onChange({ ...setup, docBudget: Number(e.target.value) })}
            />
          </label>

          <label className="row" style={{ gap: 6 }}>
            <input
              type="checkbox"
              checked={setup.replan}
              onChange={(e) => onChange({ ...setup, replan: e.target.checked })}
              style={{ width: "auto" }}
            />
            <span className="muted" style={{ fontSize: 11 }}>
              Try a second set of lookups if the first finds nothing
            </span>
          </label>

          <label className="row" style={{ gap: 6 }}>
            <input
              type="checkbox"
              checked={setup.includeActions}
              onChange={(e) => onChange({ ...setup, includeActions: e.target.checked })}
              style={{ width: "auto" }}
            />
            <span className="muted" style={{ fontSize: 11 }}>
              Tell the model about the action catalogue too (for "what can this do?")
            </span>
          </label>
        </div>
      )}
    </div>
  );
}

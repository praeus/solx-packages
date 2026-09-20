import { useState } from "react";
import type { SetupPrefs } from "../session/store";

/**
 * Per-session preferences.
 *
 * The grant is gone: the model reaches the whole catalogue, limited only by
 * the host's own exclusion policy (`tool_exclude` config rules union the
 * row's `solx:hidden` capability), which the harness applies via
 * `excludeHidden` on every catalogue search and dispatch. There is nothing
 * to fence here -- a tool that should be invisible is hidden in config, not
 * in this panel.
 *
 * What remains is seeded-at-creation preference: the memory scope and the
 * advanced settings (instructions, iteration budget, catalogue cap, tool
 * search). These describe how a session runs, never what it may reach.
 */
export function SetupPanel({
  setup,
  onChange,
  live,
}: {
  setup: SetupPrefs;
  onChange: (next: SetupPrefs) => void;
  /** True once a session exists: seeded-at-creation fields lock. */
  live: boolean;
}) {
  const [open, setOpen] = useState(!live);
  const [showAdvanced, setShowAdvanced] = useState(false);

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
          <span className="muted">{open ? "▾" : "▸"} Setup</span>
          {setup.memoryScope && <span className="chip">memory: {setup.memoryScope}</span>}
        </span>
      </button>

      {open && (
        <div className="col" style={{ gap: 8 }}>
          <label className="col" style={{ gap: 3 }}>
            <span className="muted" style={{ fontSize: 11 }}>
              Memory scope — shared with every session using the same name. Blank turns memory
              off.{live && " Fixed for this session; applies to the next one."}
            </span>
            <input
              placeholder="(off)"
              value={setup.memoryScope}
              disabled={live}
              onChange={(e) => onChange({ ...setup, memoryScope: e.target.value })}
            />
          </label>

          <button
            onClick={() => setShowAdvanced((v) => !v)}
            style={{ background: "none", border: "none", padding: 0, textAlign: "left" }}
          >
            <span className="muted" style={{ fontSize: 11 }}>
              {showAdvanced ? "▾" : "▸"} Advanced
            </span>
          </button>

          {showAdvanced && (
            <div className="col" style={{ gap: 6 }}>
              <label className="col" style={{ gap: 3 }}>
                <span className="muted" style={{ fontSize: 11 }}>
                  Instructions — the harness adds no base prompt, so this plus any
                  skills that load is the whole framing the model gets.
                  {live && " Already sent for this session; applies to the next one."}
                </span>
                <textarea
                  rows={6}
                  value={setup.system}
                  disabled={live}
                  onChange={(e) => onChange({ ...setup, system: e.target.value })}
                />
              </label>
              <div className="row" style={{ gap: 6 }}>
                <label className="col" style={{ gap: 3, flex: 1 }}>
                  <span className="muted" style={{ fontSize: 11 }}>Iterations per turn</span>
                  <input
                    type="number"
                    min={1}
                    value={setup.maxIterations}
                    onChange={(e) =>
                      onChange({ ...setup, maxIterations: Number(e.target.value) || 12 })
                    }
                  />
                </label>
                <label className="col" style={{ gap: 3, flex: 1 }}>
                  <span className="muted" style={{ fontSize: 11 }}>Catalogue cap</span>
                  <input
                    type="number"
                    min={1}
                    value={setup.catalogueCap}
                    onChange={(e) =>
                      onChange({ ...setup, catalogueCap: Number(e.target.value) || 16 })
                    }
                  />
                </label>
              </div>
              <label className="row" style={{ gap: 6 }}>
                <input
                  type="checkbox"
                  checked={setup.toolSearch}
                  onChange={(e) => onChange({ ...setup, toolSearch: e.target.checked })}
                  style={{ width: "auto" }}
                />
                <span className="muted" style={{ fontSize: 11 }}>
                  Let the model search for more tools
                </span>
              </label>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

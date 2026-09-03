import { useState } from "react";
import type { AllowEntry, ToolsPreview } from "../harness";
import type { SetupPrefs } from "../session/store";

/** Starting points for the grant. Not exhaustive -- a path can be typed. */
const PRESETS: { label: string; entry: AllowEntry }[] = [
  {
    label: "Read documents",
    entry: { path: "/builtin/document", actions: ["search_documents", "entity_get_document"] },
  },
  { label: "Write documents", entry: { path: "/builtin/document", actions: null } },
  { label: "Files", entry: { path: "/builtin/file", actions: null } },
];

/**
 * What the agent is allowed to reach.
 *
 * The grant is required, default-deny, and has no wildcard shorthand, so this
 * is a first-class surface rather than an advanced setting -- a session
 * cannot start without it.
 *
 * **It stays editable once a session is running.** The rule the gate enforces
 * is that *the model* cannot widen its own reach, not that reach is
 * immutable; the operator is the trust root. Freezing it against the operator
 * too was what made a wandering conversation impossible -- ask for a summary,
 * then ask to mail it, and the session had to be thrown away. Changing it
 * writes a system turn, so the transcript records when reach changed.
 *
 * A few fields genuinely cannot change mid-session and say so: the memory
 * scope and the instructions are both seeded into the transcript when the
 * session is created, so editing them later would describe something that
 * never happened.
 */
export function SetupPanel({
  setup,
  onChange,
  onPreview,
  live,
  queryHint,
}: {
  setup: SetupPrefs;
  onChange: (next: SetupPrefs) => void;
  onPreview: (grant: AllowEntry[], query: string | null, cap: number) => Promise<ToolsPreview>;
  /** True once a session exists: seeded-at-creation fields lock, the grant does not. */
  live: boolean;
  queryHint: string;
}) {
  const [open, setOpen] = useState(!live);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [customPath, setCustomPath] = useState("");
  const [preview, setPreview] = useState<{ tools: string[]; dropped: number } | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);

  const addEntry = (entry: AllowEntry) => {
    if (setup.grant.some((a) => a.path === entry.path && sameActions(a, entry))) return;
    onChange({ ...setup, grant: [...setup.grant, entry] });
  };

  const runPreview = async () => {
    setPreviewing(true);
    setPreviewError(null);
    setPreview(null);
    try {
      const result = await onPreview(setup.grant, queryHint || null, setup.catalogueCap);
      setPreview({
        tools: (result.tools ?? []).map((t) => t.function?.name).filter(Boolean) as string[],
        dropped: result.dropped ?? 0,
      });
    } catch (err) {
      setPreviewError(err instanceof Error ? err.message : String(err));
    } finally {
      setPreviewing(false);
    }
  };

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
          <span className="muted">{open ? "▾" : "▸"} Tools and setup</span>
          <span className="chip">{setup.grant.length} allowed</span>
          {setup.memoryScope && <span className="chip">memory: {setup.memoryScope}</span>}
        </span>
      </button>

      {open && (
        <div className="col" style={{ gap: 8 }}>
          {live && (
            <span className="faint" style={{ fontSize: 11 }}>
              Changing what is allowed takes effect on the next message, and is noted in the
              transcript. The model still cannot widen this itself.
            </span>
          )}

          <div className="col" style={{ gap: 4 }}>
            {setup.grant.map((entry, i) => (
              <div key={i} className="row" style={{ gap: 6, justifyContent: "space-between" }}>
                <span style={{ fontFamily: "var(--font-mono)", fontSize: 12, minWidth: 0 }}>
                  {entry.path}
                  {entry.actions?.length ? (
                    <span className="faint"> · {entry.actions.join(", ")}</span>
                  ) : (
                    <span className="faint"> · all actions</span>
                  )}
                </span>
                <button
                  onClick={() =>
                    onChange({ ...setup, grant: setup.grant.filter((_, j) => j !== i) })
                  }
                  style={{ padding: "1px 7px" }}
                >
                  ✕
                </button>
              </div>
            ))}
            {setup.grant.length === 0 && (
              <span className="chip danger">Nothing allowed — the agent cannot act</span>
            )}
          </div>

          <div className="row" style={{ gap: 5, flexWrap: "wrap" }}>
            {PRESETS.map((preset) => (
              <button key={preset.label} onClick={() => addEntry(preset.entry)}>
                + {preset.label}
              </button>
            ))}
          </div>
          <div className="row" style={{ gap: 5 }}>
            <input
              placeholder="/packages/solx-google"
              value={customPath}
              onChange={(e) => setCustomPath(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && customPath.trim()) {
                  addEntry({ path: customPath.trim(), actions: null });
                  setCustomPath("");
                }
              }}
            />
            <button
              disabled={!customPath.trim()}
              onClick={() => {
                addEntry({ path: customPath.trim(), actions: null });
                setCustomPath("");
              }}
            >
              Add
            </button>
          </div>

          <div className="row" style={{ gap: 6 }}>
            <button onClick={runPreview} disabled={previewing || setup.grant.length === 0}>
              {previewing ? "Resolving…" : "Preview catalogue"}
            </button>
            <span className="faint" style={{ fontSize: 11 }}>
              What the model would actually get, without spending a model call.
            </span>
          </div>
          {previewError && <span className="chip danger">{previewError}</span>}
          {preview && (
            <div className="col" style={{ gap: 3 }}>
              {preview.tools.length === 0 ? (
                <span className="chip danger">No tools resolved — widen what is allowed</span>
              ) : (
                preview.tools.map((name) => (
                  <span key={name} style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}>
                    {name}
                  </span>
                ))
              )}
              {preview.dropped > 0 && (
                <span className="chip warn">
                  {preview.dropped} matched but fell outside the cap
                </span>
              )}
            </div>
          )}

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
                  Instructions — the model gets no other framing, so this is the whole
                  behavioural contract.
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
                  Let the model search for more tools (within the same grant)
                </span>
              </label>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function sameActions(a: AllowEntry, b: AllowEntry): boolean {
  const x = a.actions ?? null;
  const y = b.actions ?? null;
  if (x === null || y === null) return x === y;
  return x.length === y.length && x.every((v, i) => v === y[i]);
}

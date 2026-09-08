import { useEffect, useState } from "react";
import type { AllowEntry, PathSuggestion, ToolsPreview } from "../harness";
import type { SetupPrefs } from "../session/store";

/**
 * Starting points for the grant. Not exhaustive -- a path can be typed.
 *
 * "All tools" is first because it is the default (see `DEFAULT_GRANT`) and
 * the way back to it after narrowing: `*` is a real pattern the gate
 * understands, not a placeholder. The narrower presets are for sessions an
 * operator wants deliberately fenced.
 *
 * "Read the catalogue" names its actions explicitly rather than passing
 * null: the write and async halves of /builtin/action are hard-denied
 * regardless of grant shape, so a null here would advertise reach the gate
 * will refuse. A Command row like solx-quickjs's builders needs no such
 * carve-out -- a plain path grant reaches it the same as any other action
 * type, and it still suspends for approval before it runs, since every
 * Command/Webhook row is unconditionally destructive.
 */
const PRESETS: { label: string; entry: AllowEntry }[] = [
  { label: "All tools", entry: { path: "*", actions: null } },
  {
    label: "Read documents",
    entry: { path: "/builtin/document", actions: ["search_documents", "entity_get_document"] },
  },
  { label: "Write documents", entry: { path: "/builtin/document", actions: null } },
  { label: "Files", entry: { path: "/builtin/file", actions: null } },
  { label: "Types", entry: { path: "/builtin/type", actions: null } },
  {
    label: "Read the catalogue",
    entry: {
      path: "/builtin/action",
      actions: ["search_actions", "entity_get_action", "entity_list_actions"],
    },
  },
  { label: "Build JS actions", entry: { path: "/packages/solx-quickjs", actions: null } },
];

/**
 * What the agent is allowed to reach.
 *
 * The grant is required and defaults to `*` -- the whole catalogue -- so a
 * session is useful without the operator enumerating paths first. This stays
 * a first-class surface rather than an advanced setting because narrowing it
 * is the point: it is where an operator fences a session that should not see
 * everything, and it is visible enough that a wide grant is never a surprise.
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
  onSearchPaths,
  live,
  queryHint,
}: {
  setup: SetupPrefs;
  onChange: (next: SetupPrefs) => void;
  onPreview: (grant: AllowEntry[], query: string | null, cap: number) => Promise<ToolsPreview>;
  /** Distinct action paths matching a partial query, for the path picker below. */
  onSearchPaths: (q: string) => Promise<PathSuggestion[]>;
  /** True once a session exists: seeded-at-creation fields lock, the grant does not. */
  live: boolean;
  queryHint: string;
}) {
  const [open, setOpen] = useState(!live);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [pathQuery, setPathQuery] = useState("");
  const [pathOpen, setPathOpen] = useState(false);
  const [suggestions, setSuggestions] = useState<PathSuggestion[]>([]);
  const [searching, setSearching] = useState(false);
  const [preview, setPreview] = useState<{ tools: string[]; dropped: number } | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);

  const addEntry = (entry: AllowEntry) => {
    if (setup.grant.some((a) => a.path === entry.path && sameActions(a, entry))) return;
    onChange({ ...setup, grant: [...setup.grant, entry] });
  };

  const pickPath = (path: string) => {
    addEntry({ path, actions: null });
    setPathQuery("");
    setSuggestions([]);
    setPathOpen(false);
  };

  // Debounced live search over the action catalogue's paths, so typing
  // "firefox" finds "/packages/solx-firefox" without the operator needing to
  // already know it exists.
  useEffect(() => {
    const q = pathQuery.trim();
    if (!q) {
      setSuggestions([]);
      setSearching(false);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const timer = setTimeout(() => {
      onSearchPaths(q)
        .then((results) => {
          if (!cancelled) setSuggestions(results);
        })
        .catch(() => {
          if (!cancelled) setSuggestions([]);
        })
        .finally(() => {
          if (!cancelled) setSearching(false);
        });
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [pathQuery, onSearchPaths]);

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

          <div className="col" style={{ gap: 0, position: "relative" }}>
            <input
              placeholder="Add a path… try “firefox”"
              value={pathQuery}
              onChange={(e) => setPathQuery(e.target.value)}
              onFocus={() => setPathOpen(true)}
              onBlur={() => setTimeout(() => setPathOpen(false), 150)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  const first = suggestions[0];
                  if (first) pickPath(first.path);
                  else if (pathQuery.trim()) pickPath(pathQuery.trim());
                } else if (e.key === "Escape") {
                  setPathOpen(false);
                }
              }}
            />
            {pathOpen && (
              <div
                className="col"
                style={{
                  gap: 2,
                  position: "absolute",
                  top: "100%",
                  left: 0,
                  right: 0,
                  zIndex: 1,
                  marginTop: 3,
                  padding: 4,
                  border: "1px solid var(--border)",
                  borderRadius: "var(--radius)",
                  background: "var(--bg-raised)",
                  maxHeight: 180,
                  overflowY: "auto",
                }}
              >
                {!pathQuery.trim() &&
                  PRESETS.map((preset) => (
                    <button
                      key={preset.label}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => {
                        addEntry(preset.entry);
                        setPathOpen(false);
                      }}
                      style={{ textAlign: "left", padding: "2px 5px" }}
                    >
                      + {preset.label}{" "}
                      <span className="faint" style={{ fontFamily: "var(--font-mono)" }}>
                        {preset.entry.path}
                      </span>
                    </button>
                  ))}
                {pathQuery.trim() && searching && (
                  <span className="faint" style={{ fontSize: 11, padding: "2px 5px" }}>
                    Searching…
                  </span>
                )}
                {pathQuery.trim() &&
                  !searching &&
                  suggestions.map((s) => (
                    <button
                      key={s.path}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => pickPath(s.path)}
                      style={{
                        textAlign: "left",
                        padding: "2px 5px",
                        fontFamily: "var(--font-mono)",
                        fontSize: 12,
                      }}
                    >
                      {s.path}{" "}
                      <span className="faint" style={{ fontFamily: "var(--font-sans, inherit)" }}>
                        · {s.count} action{s.count === 1 ? "" : "s"}
                      </span>
                    </button>
                  ))}
                {pathQuery.trim() && !searching && (
                  <button
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => pickPath(pathQuery.trim())}
                    style={{ textAlign: "left", padding: "2px 5px" }}
                  >
                    <span className="faint" style={{ fontSize: 11 }}>
                      {suggestions.length === 0
                        ? "No matching path — use literal:"
                        : "Or use literal path:"}
                    </span>{" "}
                    <span style={{ fontFamily: "var(--font-mono)", fontSize: 12 }}>
                      {pathQuery.trim()}
                    </span>
                  </button>
                )}
              </div>
            )}
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
                // Which of the two it is matters: telling someone to widen a
                // grant that is already `*` is what sent them looking for a
                // wildcard that was there all along.
                <span className="chip danger">
                  {queryHint.trim()
                    ? `Nothing matched “${queryHint.trim()}” in what is allowed`
                    : "No tools resolved — widen what is allowed"}
                </span>
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

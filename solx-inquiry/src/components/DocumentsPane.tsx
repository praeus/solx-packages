import { useEffect, useRef, useState } from "react";
import type { GatheredDoc } from "../harness";

function pretty(value: unknown): string {
  try {
    if (value === null || value === undefined) return "";
    return typeof value === "string" ? value : JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/**
 * The documents an answer was grounded in.
 *
 * Collapsed by default and closed on demand, because this is evidence rather
 * than the answer: a reader usually wants to know *that* eleven documents were
 * read and *which* ones, and only occasionally wants to read them. The ref is
 * always visible -- it is what the answer cites and what `solx get doc` wants.
 *
 * `collapseSignal` folds every pane when the thread's "Collapse all" is
 * pressed, the same mechanism the agent uses for its system notes.
 */
export function DocumentsPane({
  documents,
  collapseSignal,
}: {
  documents: GatheredDoc[];
  collapseSignal?: number;
}) {
  const [open, setOpen] = useState(false);
  const prev = useRef(collapseSignal);

  useEffect(() => {
    if (collapseSignal !== prev.current) {
      prev.current = collapseSignal;
      setOpen(false);
    }
  }, [collapseSignal]);

  return (
    <div className="col" style={{ gap: 4 }}>
      <button
        onClick={() => setOpen((v) => !v)}
        style={{ background: "none", border: "none", padding: 0, textAlign: "left" }}
      >
        <span className="chip">
          {open ? "▾" : "▸"} {documents.length} document{documents.length === 1 ? "" : "s"} read
        </span>
      </button>

      {open &&
        documents.map((doc) => (
          <div
            key={doc.ref}
            style={{
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              background: "var(--bg-raised)",
              padding: "5px 7px",
            }}
          >
            <div style={{ fontFamily: "var(--font-mono)", fontSize: 12 }}>{doc.ref}</div>
            {doc.title && <div className="muted" style={{ fontSize: 11 }}>{doc.title}</div>}
            {doc.type_ref && (
              <div className="faint" style={{ fontSize: 11, fontFamily: "var(--font-mono)" }}>
                {doc.type_ref}
              </div>
            )}
            {doc.summary && (
              <div style={{ fontSize: 12, marginTop: 3 }}>{doc.summary}</div>
            )}
            {pretty(doc.contents) && (
              <pre style={{ maxHeight: 200, overflowY: "auto", marginTop: 4 }}>
                {pretty(doc.contents)}
              </pre>
            )}
          </div>
        ))}
    </div>
  );
}

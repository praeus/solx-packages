import { useState } from "react";
import type { Host } from "../../../solx-widgets/src/wrap/host";
import { isRunnableActionHit } from "../dispatch";
import type { InquireHit, Turn } from "../types";

/**
 * One rendered turn. User turns and chat turns are simple text. Inquire
 * turns render the summary plus a list of action hits, each with a "Run"
 * button that dispatches `actions.exec` with the hit's params.
 *
 * Error turns surface the message in the danger chip so the user can see
 * what went wrong and try again.
 */
export function TurnBlock({
  turn,
  host,
  onRun,
}: {
  turn: Turn;
  host: Host | null;
  onRun: (hit: InquireHit, params: Record<string, unknown>, status: "ok" | "error", message: string) => void;
}) {
  if (turn.kind === "user") {
    return (
      <div className="col" style={{ gap: 4, alignItems: "flex-end" }}>
        <div
          className="col"
          style={{
            gap: 4,
            background: "var(--accent-soft)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius)",
            padding: "6px 10px",
            maxWidth: "85%",
          }}
        >
          <span style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{turn.text}</span>
        </div>
        <span className="faint" style={{ fontSize: 11 }}>{formatTime(turn.at)}</span>
      </div>
    );
  }

  if (turn.kind === "chat") {
    return (
      <div className="col" style={{ gap: 4 }}>
        <div
          className="col"
          style={{
            gap: 4,
            background: "var(--bg-raised)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius)",
            padding: "6px 10px",
            maxWidth: "85%",
          }}
        >
          <div className="row" style={{ gap: 6, alignItems: "baseline" }}>
            <strong style={{ fontSize: 12 }}>Assistant</strong>
            <span className="faint" style={{ fontSize: 11 }}>{turn.model}</span>
          </div>
          <span style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{turn.text || "(no content)"}</span>
        </div>
        <span className="faint" style={{ fontSize: 11 }}>{formatTime(turn.at)}</span>
      </div>
    );
  }

  if (turn.kind === "error") {
    return (
      <div className="col" style={{ gap: 4 }}>
        <div
          className="chip danger"
          style={{ alignSelf: "flex-start", padding: "6px 10px" }}
        >
          {turn.message}
        </div>
        <span className="faint" style={{ fontSize: 11 }}>{formatTime(turn.at)}</span>
      </div>
    );
  }

  // turn.kind === "inquire"
  const actionHits = turn.result.hits.filter(isRunnableActionHit);
  const docHits = turn.result.hits.filter((h) => h.source === "document");
  return (
    <div className="col" style={{ gap: 6 }}>
      <div
        className="col"
        style={{
          gap: 6,
          background: "var(--bg-raised)",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius)",
          padding: "8px 10px",
        }}
      >
        <div className="row" style={{ gap: 6, alignItems: "baseline" }}>
          <strong style={{ fontSize: 12 }}>Research</strong>
          <span className="faint" style={{ fontSize: 11 }}>
            {turn.model} · {turn.result.scope} · {turn.result.terms.join(", ") || "no terms"}
          </span>
        </div>
        <span style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
          {turn.result.summary || "(no summary)"}
        </span>
        {actionHits.length > 0 && (
          <div className="col" style={{ gap: 4, marginTop: 4 }}>
            <span className="muted" style={{ fontSize: 11 }}>
              Suggested actions ({actionHits.length})
            </span>
            {actionHits.map((hit, i) => (
              <ActionHitRow
                key={i}
                hit={hit}
                host={host}
                onRun={onRun}
              />
            ))}
          </div>
        )}
        {docHits.length > 0 && (
          <div className="col" style={{ gap: 2, marginTop: 4 }}>
            <span className="muted" style={{ fontSize: 11 }}>
              Cited documents ({docHits.length})
            </span>
            {docHits.slice(0, 5).map((hit, i) => (
              <span key={i} className="faint" style={{ fontSize: 11 }}>
                · {hit.path}/{hit.name}
                {hit.title ? ` — ${hit.title}` : ""}
              </span>
            ))}
            {docHits.length > 5 && (
              <span className="faint" style={{ fontSize: 11 }}>
                · …and {docHits.length - 5} more
              </span>
            )}
          </div>
        )}
      </div>
      <span className="faint" style={{ fontSize: 11 }}>{formatTime(turn.at)}</span>
    </div>
  );
}

function ActionHitRow({
  hit,
  host,
  onRun,
}: {
  hit: InquireHit;
  host: Host | null;
  onRun: (hit: InquireHit, params: Record<string, unknown>, status: "ok" | "error", message: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);

  const run = async () => {
    if (!host) return;
    setBusy(true);
    try {
      // inquire returns paramSchema when available, but not always a
      // populated params object — leave the user to fill in a params JSON
      // blob if there's no obvious default. The action will validate on
      // exec regardless.
      const params = promptForParams(hit);
      if (params === null) return; // user cancelled
      const r = await host.try<unknown>(hit.path + "/" + hit.name, params);
      if (r.ok) {
        onRun(hit, params, "ok", "ran " + hit.name);
      } else {
        onRun(hit, params, "error", r.error);
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="col" style={{ gap: 2 }}>
      <div className="row" style={{ gap: 6 }}>
        <button
          disabled={!host || busy}
          onClick={run}
          style={{ fontSize: 11, padding: "2px 8px" }}
          title={hit.path + "/" + hit.name}
        >
          {busy ? "…" : "Run"}
        </button>
        <span style={{ fontSize: 12 }}>{hit.path}/{hit.name}</span>
        <span className="chip" style={{ fontSize: 10 }}>{hit.details?.category ?? "action"}</span>
        {typeof hit.score === "number" && (
          <span className="faint" style={{ fontSize: 10 }}>
            score {hit.score.toFixed(2)}
          </span>
        )}
      </div>
      {open && (
        <pre style={{ fontSize: 11 }}>{JSON.stringify(hit.details ?? {}, null, 2)}</pre>
      )}
      <button
        onClick={() => setOpen((v) => !v)}
        style={{ alignSelf: "flex-start", fontSize: 10, padding: "0 4px", background: "transparent", border: "none", color: "var(--text-faint)" }}
      >
        {open ? "hide details" : "show details"}
      </button>
    </div>
  );
}

/**
 * Ask the user for parameters as a JSON blob. inquiry surfaces the schema
 * but doesn't always supply default values; rather than guess, surface a
 * tiny editor and let the user paste in whatever the action expects.
 *
 * Returns `null` if the user cancels.
 */
function promptForParams(hit: InquireHit): Record<string, unknown> | null {
  const defaultText = JSON.stringify({}, null, 2);
  // Use a synchronous prompt for the scaffold — replace with an inline
  // editor if/when a real params form is built.
  // eslint-disable-next-line no-alert
  const text = window.prompt(
    "Parameters for " + hit.path + "/" + hit.name + " (JSON):",
    defaultText,
  );
  if (text === null) return null;
  try {
    const parsed = JSON.parse(text);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
  } catch {
    // fall through
  }
  return {};
}

function formatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleTimeString();
  } catch {
    return iso;
  }
}

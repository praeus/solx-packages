import { useState } from "react";
import type { Host } from "../../../solx-widgets/src/wrap/host";
import { isRunnableActionHit } from "../dispatch";
import type { InquireHit, MultiInquireScript, Turn } from "../types";

/**
 * One rendered turn. User turns are simple text. Answer turns render every
 * response multi_inquire produced, plus action hits (each with a "Run"
 * button), cited documents, and any proposed action plans — read-only for
 * now, see MultiInquireScript's rendering below.
 *
 * Run turns and error turns both surface in a chip, but only error turns use
 * the danger styling — a successful "Run" is not a failure and must not look
 * like one.
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

  if (turn.kind === "run") {
    return (
      <div className="col" style={{ gap: 4 }}>
        <div
          className={"chip " + (turn.status === "ok" ? "ok" : "danger")}
          style={{ alignSelf: "flex-start", padding: "6px 10px" }}
        >
          {turn.message}
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

  // turn.kind === "answer"
  const { result } = turn;
  const mode = result.intent.mode;
  const actionHits = result.hits.filter(isRunnableActionHit);
  const docHits = result.hits.filter((h) => h.source === "document");
  const citedResponses = result.responses.filter((r) => r.citations.length > 0);
  const text = result.responses.map((r) => r.text).join("\n\n") || "(no response)";

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
        <div className="row" style={{ gap: 6, alignItems: "baseline", flexWrap: "wrap" }}>
          <strong style={{ fontSize: 12 }}>Assistant</strong>
          <span className="faint" style={{ fontSize: 11 }}>{turn.model}</span>
          <span className={"chip" + (mode === "inquire" ? " accent" : "")} style={{ fontSize: 10 }}>
            {mode === "inquire" ? "researched" : "direct"}
          </span>
        </div>
        <span style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{text}</span>

        {citedResponses.length > 0 && (
          <div className="col" style={{ gap: 2, marginTop: 2 }}>
            {citedResponses.map((r, i) => (
              <span key={i} className="faint" style={{ fontSize: 11 }}>
                cites: {r.citations.join(", ")}
              </span>
            ))}
          </div>
        )}

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

        {result.scripts.length > 0 && (
          <div className="col" style={{ gap: 4, marginTop: 4 }}>
            <span className="muted" style={{ fontSize: 11 }}>
              Proposed action plan{result.scripts.length > 1 ? "s" : ""} ({result.scripts.length}) — not run automatically
            </span>
            {result.scripts.map((script, i) => (
              <ScriptRow key={i} script={script} />
            ))}
          </div>
        )}

        {result.notes.length > 0 && (
          <div className="col" style={{ gap: 2, marginTop: 4 }}>
            {result.notes.map((note, i) => (
              <span key={i} className="faint" style={{ fontSize: 11 }}>
                note: {note}
              </span>
            ))}
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
      // multi_inquire returns paramSchema when available, but not always a
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
 * A `multi_inquire` script, rendered read-only: title, whether it carries
 * anything destructive, and its steps behind a details toggle. Nothing here
 * executes a step — see the roadmap's "Auto-run non-destructive actions"
 * item for what a "Run plan" affordance would need.
 */
function ScriptRow({ script }: { script: MultiInquireScript }) {
  const [open, setOpen] = useState(false);

  return (
    <div className="col" style={{ gap: 2 }}>
      <div className="row" style={{ gap: 6 }}>
        <span style={{ fontSize: 12 }}>{script.title}</span>
        {script.destructive.length > 0 && (
          <span className="chip warn" style={{ fontSize: 10 }}>destructive</span>
        )}
        <span className="faint" style={{ fontSize: 10 }}>{script.steps.length} step(s)</span>
      </div>
      {open && (
        <pre style={{ fontSize: 11 }}>{JSON.stringify(script.steps, null, 2)}</pre>
      )}
      <button
        onClick={() => setOpen((v) => !v)}
        style={{ alignSelf: "flex-start", fontSize: 10, padding: "0 4px", background: "transparent", border: "none", color: "var(--text-faint)" }}
      >
        {open ? "hide steps" : "show steps"}
      </button>
    </div>
  );
}

/**
 * Ask the user for parameters as a JSON blob. multi_inquire surfaces the
 * schema but doesn't always supply default values; rather than guess,
 * surface a tiny editor and let the user paste in whatever the action
 * expects.
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

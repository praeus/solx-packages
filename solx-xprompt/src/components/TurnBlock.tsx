import { useState } from "react";
import type { Host } from "../../../solx-widgets/src/wrap/host";
import { isDestructiveHit, isRunnableActionHit, unapprovedDestructiveRefs } from "../dispatch";
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
/** Chip variant per run-turn status. `skipped` reads as a caution, not a pass. */
const RUN_STATUS_CLASS: Record<"ok" | "error" | "skipped", string> = {
  ok: "ok",
  error: "danger",
  skipped: "warn",
};

export function TurnBlock({
  turn,
  host,
  onRun,
  onRunScript,
  approved,
}: {
  turn: Turn;
  host: Host | null;
  onRun: (hit: InquireHit, params: Record<string, unknown>, status: "ok" | "error", message: string) => void;
  /**
   * Auto-run the entire script in order. Captures are substituted between
   * steps; destructive steps are gated against `approved`. Pushes one
   * run-turn per step through the same `onRun` channel.
   */
  onRunScript?: (script: MultiInquireScript) => void;
  /** Session-scoped allowlist of destructive action refs the user has approved. */
  approved?: ReadonlySet<string>;
}) {
  // Hooks must run unconditionally on every render, so this is declared
  // before the early returns below even though only the "answer" branch
  // reads it.
  const [citesOpen, setCitesOpen] = useState(false);

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
          className={"chip " + RUN_STATUS_CLASS[turn.status]}
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
  // Default closed — a 4-turn transcript used to stack four "Cited
  // documents (N)" blocks and bury the answer. The header still shows
  // the count, so the user can expand whichever turn they're reading.

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
            {/* Suggested actions are search results, not a plan - nothing
                chose them or their parameters - so each is run individually,
                with parameters the user supplies. Running the list wholesale
                is what `scripts[]` and "Run plan" are for. */}
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
            <button
              onClick={() => setCitesOpen((o) => !o)}
              className="muted"
              style={{
                fontSize: 11,
                background: "none",
                border: "none",
                padding: 0,
                textAlign: "left",
                cursor: "pointer",
                color: "var(--text-muted)",
              }}
            >
              {citesOpen ? "▾" : "▸"} Cited documents ({docHits.length})
            </button>
            {citesOpen && (
              <div className="col" style={{ gap: 2, marginTop: 2 }}>
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
        )}

        {result.scripts.length > 0 && (
          <div className="col" style={{ gap: 4, marginTop: 4 }}>
            <span className="muted" style={{ fontSize: 11 }}>
              Proposed action plan{result.scripts.length > 1 ? "s" : ""} ({result.scripts.length})
            </span>
            {result.scripts.map((script, i) => (
              <ScriptRow
                key={i}
                script={script}
                host={host}
                onRunScript={onRunScript}
                approved={approved}
              />
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
  const destructive = isDestructiveHit(hit);

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
          title={
            destructive
              ? `${hit.path}/${hit.name} — destructive: solx would stop for a human decision`
              : hit.path + "/" + hit.name
          }
        >
          {busy ? "…" : "Run"}
        </button>
        <span style={{ fontSize: 12 }}>{hit.path}/{hit.name}</span>
        <span className="chip" style={{ fontSize: 10 }}>{hit.details?.category ?? "action"}</span>
        {/* Running one hit by hand is a deliberate act - the user picks it and
            types its parameters - so it is not gated the way an unattended
            plan is. But nothing here previously said that a `command` action
            is shell execution, and that is worth seeing before clicking. */}
        {destructive && (
          <span
            className="chip warn"
            style={{ fontSize: 10 }}
            title={`${hit.details?.actionType ?? "this action"} — solx marks this destructive`}
          >
            destructive
          </span>
        )}
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
 * anything destructive, and its steps behind a details toggle. The
 * `onRunScript` callback wires a "Run plan" button that drives
 * `dispatch.runScript` (with capture substitution) against the host.
 */
function ScriptRow({
  script,
  host,
  onRunScript,
  approved,
}: {
  script: MultiInquireScript;
  host: Host | null;
  onRunScript?: (script: MultiInquireScript) => void;
  approved?: ReadonlySet<string>;
}) {
  const [open, setOpen] = useState(false);
  const unapproved = approved ? unapprovedDestructiveRefs(script, approved) : script.destructive;

  return (
    <div className="col" style={{ gap: 2 }}>
      <div className="row" style={{ gap: 6 }}>
        <span style={{ fontSize: 12 }}>{script.title ?? "(untitled plan)"}</span>
        {script.destructive.length > 0 && (
          <span className="chip warn" style={{ fontSize: 10 }}>
            destructive{script.destructive.length > 1 ? ` (${script.destructive.length})` : ""}
          </span>
        )}
        <span className="faint" style={{ fontSize: 10 }}>{script.steps.length} step(s)</span>
        {onRunScript && (
          <button
            disabled={!host}
            onClick={() => onRunScript(script)}
            title={unapproved.length > 0
              ? `Asks first: ${unapproved.length} destructive action(s) not yet approved`
              : `Run ${script.steps.length} step(s) in order`}
            style={{ fontSize: 11, padding: "2px 8px", marginLeft: "auto" }}
          >
            Run plan{unapproved.length > 0 ? "…" : ""}
          </button>
        )}
      </div>
      {unapproved.length > 0 && (
        <span className="faint" style={{ fontSize: 10 }}>
          asks before running: {unapproved.join(", ")}
        </span>
      )}
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
 * Returns `null` if the user cancels — including when `window.prompt` is
 * unavailable (Electrobun, certain iframes, JSDOM). We must not fail open
 * there: `ActionHitRow.run` treats anything but `null` as user-supplied
 * params and executes immediately, so a fallback like `{}` would let any
 * action — destructive ones included — run with zero chance to review or
 * cancel it.
 */
function promptForParams(hit: InquireHit): Record<string, unknown> | null {
  const defaultText = JSON.stringify({}, null, 2);
  let text: string | null;
  try {
    // eslint-disable-next-line no-alert
    text = window.prompt(
      "Parameters for " + hit.path + "/" + hit.name + " (JSON):",
      defaultText,
    );
  } catch {
    return null;
  }
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

/**
 * Blank for a turn reloaded from session history: solx-inquiry stores no
 * per-turn timestamp (see `StoredMultiInquireTurn`), so `at` is `""` there
 * rather than an invalid date rendered as "Invalid Date".
 */
function formatTime(iso: string): string {
  if (!iso) return "";
  try {
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString();
  } catch {
    return "";
  }
}

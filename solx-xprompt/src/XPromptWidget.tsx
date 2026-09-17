import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSolxWidgetClient } from "../../solx-widgets/src/wrap/SolxWidgetContext";
import { hostFromClient, type Host } from "../../solx-widgets/src/wrap/host";
import { dispatch } from "./dispatch";
import { LIST_MODELS, XPROMPT_SESSION_PATH } from "./refs";
import { listSessions, loadSessionTranscript, randomSessionName, saveSessionDocument } from "./session";
import type { InquireHit, OllamaModel, Turn, XPromptSessionSummary } from "./types";
import { Composer } from "./components/Composer";
import { TurnBlock } from "./components/TurnBlock";
import { ConsolePanel } from "./components/ConsolePanel";

export interface XPromptWidgetFields {
  /** Optional heading. */
  title?: string;
  /** Open straight into a particular model; otherwise picks the first listed. */
  model?: string;
}

const LS_MODEL = "solx-xprompt.model";
const LS_TRANSCRIPT = "solx-xprompt.transcript";
const LS_SESSION_NAME = "solx-xprompt.sessionName";

type Tab = "chat" | "console";

/**
 * ExecPrompt (XPrompt): turn a natural-language instruction into a
 * multi_inquire turn — a direct answer or a researched one, whichever the
 * instruction needs — with the merged console of that turn's own phases one
 * tab away.
 *
 * Every turn runs under one named session (`session.ts`): a solx-names name,
 * generated once and kept in localStorage so a reload stays on the same
 * session. After each successful turn the `session_document` multi_inquire
 * returned is saved back to `/xprompt/sessions/<name>` — that's what lets
 * the *next* turn's intent phase actually see prior history instead of
 * always looking like a first message — and the same name doubles as the
 * merged-console `logId`, so the call log lands at
 * `/xprompt/call-logs/<name>`, right alongside the session document it
 * belongs to. The session picker below reads other sessions back by
 * reconstructing a transcript from their saved `turns[]` (see
 * `loadSessionTranscript`); the widget's own localStorage transcript is
 * only ever the *current* session's fast local cache.
 */
export function XPromptWidget({ fields }: { fields: XPromptWidgetFields | undefined }) {
  const client = useSolxWidgetClient();
  const host: Host | null = useMemo(
    () => (client ? hostFromClient(client) : null),
    [client],
  );

  const [models, setModels] = useState<OllamaModel[]>([]);
  const [model, setModel] = useState<string>(() => {
    if (fields?.model) return fields.model;
    try {
      return localStorage.getItem(LS_MODEL) || "";
    } catch {
      return "";
    }
  });
  const [transcript, setTranscript] = useState<Turn[]>(() => loadTranscript());
  const [sessionName, setSessionName] = useState<string>(() => {
    try {
      return localStorage.getItem(LS_SESSION_NAME) || "";
    } catch {
      return "";
    }
  });
  const [sessions, setSessions] = useState<XPromptSessionSummary[]>([]);
  const [tab, setTab] = useState<Tab>("chat");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const sessionRef = useMemo(() => `${XPROMPT_SESSION_PATH}/${sessionName}`, [sessionName]);

  // Bumped on every send/stop/session-switch. A new turn (or a switch away
  // from the session it belongs to) abandons any earlier one that might
  // still be settling — same pattern as solx-agent's genRef.
  const genRef = useRef(0);
  // The in-flight turn's invocation id, so Stop can cancel it for real via
  // client.invocations.stop rather than just abandoning the UI's wait.
  const invocationIdRef = useRef<string | null>(null);
  const threadRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    try {
      localStorage.setItem(LS_MODEL, model);
    } catch {
      /* localStorage is best-effort */
    }
  }, [model]);

  useEffect(() => {
    try {
      localStorage.setItem(LS_TRANSCRIPT, JSON.stringify(transcript));
    } catch {
      /* best-effort */
    }
  }, [transcript]);

  // A brand-new widget instance (first ever open, or localStorage cleared)
  // has no session name yet — mint one. Nothing is saved until the first
  // turn completes, matching the call log's own lazy-create behaviour.
  useEffect(() => {
    if (!host || sessionName) return;
    let cancelled = false;
    void randomSessionName(host).then((name) => {
      if (cancelled) return;
      setSessionName(name);
      try {
        localStorage.setItem(LS_SESSION_NAME, name);
      } catch {
        /* best-effort */
      }
    });
    return () => {
      cancelled = true;
    };
  }, [host, sessionName]);

  const refreshSessions = useCallback(() => {
    if (!host) return;
    void listSessions(host).then(setSessions);
  }, [host]);

  useEffect(() => {
    refreshSessions();
  }, [refreshSessions]);

  // Pull the model list once the client is up. A failure here means
  // ollama isn't installed or reachable — show a banner, don't crash.
  useEffect(() => {
    if (!host) return;
    let cancelled = false;
    void host
      .try<{ models?: OllamaModel[] }>(LIST_MODELS, {})
      .then((r) => {
        if (cancelled) return;
        const list = (r.ok && r.value?.models ? r.value.models : []).slice();
        list.sort((a, b) => a.name.localeCompare(b.name));
        setModels(list);
        setModel((current) =>
          current && list.some((m) => m.name === current) ? current : list[0]?.name || "",
        );
      });
    return () => {
      cancelled = true;
    };
  }, [host]);

  // Follow the thread tail unless the user is reading upward.
  useEffect(() => {
    if (tab !== "chat") return;
    const el = threadRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [transcript, busy, tab]);

  const onRun = useCallback(
    (hit: InquireHit, params: Record<string, unknown>, status: "ok" | "error", message: string) => {
      const text =
        status === "ok"
          ? `Ran ${hit.path}/${hit.name} -> ok\n${summarizeResult(hit, params)}`
          : `Ran ${hit.path}/${hit.name} -> ${message}`;
      setTranscript((prev) => [
        ...prev,
        { kind: "run", status, message: text, at: new Date().toISOString() } as Turn,
      ]);
    },
    [],
  );

  const send = useCallback(
    (message: string) => {
      if (!host || !client || !model || !sessionName) return;
      const myGen = ++genRef.current;
      const at = new Date().toISOString();
      setTranscript((prev) => [...prev, { kind: "user", text: message, at }]);
      setBusy(true);
      setError(null);
      void (async () => {
        try {
          const handle = await dispatch(client, host, sessionName, message, model, sessionRef);
          if (genRef.current !== myGen) return;
          invocationIdRef.current = handle.invocationId;
          const result = await handle.result;
          if (genRef.current !== myGen) return;
          setTranscript((prev) => [
            ...prev,
            { kind: "answer", model, result, at: new Date().toISOString() },
          ]);
          // Never saved by multi_inquire itself — persisting it here is what
          // gives the *next* turn's intent phase this one's history to read.
          if (result.session_document) {
            try {
              await saveSessionDocument(host, result.session_document);
              refreshSessions();
            } catch (err) {
              // The turn itself still succeeded — a failed save shouldn't
              // turn a good answer into an error turn, just get surfaced.
              if (genRef.current === myGen) {
                setError("Session not saved: " + (err instanceof Error ? err.message : String(err)));
              }
            }
          }
        } catch (err) {
          if (genRef.current !== myGen) return;
          setError(err instanceof Error ? err.message : String(err));
          setTranscript((prev) => [
            ...prev,
            { kind: "error", message: err instanceof Error ? err.message : String(err), at: new Date().toISOString() },
          ]);
        } finally {
          if (genRef.current === myGen) {
            setBusy(false);
            invocationIdRef.current = null;
          }
        }
      })();
    },
    [host, client, model, sessionName, sessionRef, refreshSessions],
  );

  const stop = useCallback(() => {
    genRef.current++;
    setBusy(false);
    const id = invocationIdRef.current;
    invocationIdRef.current = null;
    if (client && id) {
      void client.invocations.stop(id).catch(() => {
        /* best-effort — the widget has already stopped waiting on it either way */
      });
    }
  }, [client]);

  const clear = useCallback(() => {
    genRef.current++;
    setTranscript([]);
    setError(null);
  }, []);

  // Start a new, unrelated session: a fresh name, an empty transcript. The
  // old session's document (if it ever got one) is untouched — switching
  // away doesn't delete or rename anything.
  const newSession = useCallback(() => {
    if (!host) return;
    genRef.current++;
    setBusy(false);
    invocationIdRef.current = null;
    setTranscript([]);
    setError(null);
    void randomSessionName(host).then((name) => {
      setSessionName(name);
      try {
        localStorage.setItem(LS_SESSION_NAME, name);
        localStorage.setItem(LS_TRANSCRIPT, "[]");
      } catch {
        /* best-effort */
      }
    });
  }, [host]);

  // Switch to an existing session from the picker: abandon whatever this
  // widget instance was doing, load that session's saved turns back as a
  // transcript (see loadSessionTranscript's caveats — no hits, no per-turn
  // model/timestamp), and make it the one new turns are sent under.
  const selectSession = useCallback(
    (name: string) => {
      if (!host || !name || name === sessionName) return;
      genRef.current++;
      setBusy(false);
      invocationIdRef.current = null;
      setError(null);
      setSessionName(name);
      try {
        localStorage.setItem(LS_SESSION_NAME, name);
      } catch {
        /* best-effort */
      }
      void loadSessionTranscript(host, name).then((turns) => {
        setTranscript(turns);
        try {
          localStorage.setItem(LS_TRANSCRIPT, JSON.stringify(turns));
        } catch {
          /* best-effort */
        }
      });
    },
    [host, sessionName],
  );

  if (!client || !host) {
    return (
      <div className="muted" style={{ padding: 10 }}>
        No solx client - this widget needs a host that supplies one.
      </div>
    );
  }

  const noModels = models.length === 0;
  const sessionReady = !!sessionName;

  return (
    <div
      className="col"
      style={{ gap: 8, padding: 10, height: "100%", boxSizing: "border-box" }}
    >
      <div className="row" style={{ gap: 8, alignItems: "baseline", flexWrap: "wrap" }}>
        <strong>{fields?.title ?? "XPrompt"}</strong>
        <span className="muted" style={{ fontSize: 11 }}>
          ExecPrompt - ask anything, multi_inquire decides whether to look things up
        </span>
      </div>

      <div className="row" style={{ gap: 6, alignItems: "center", flexWrap: "wrap" }}>
        <label className="muted" style={{ fontSize: 11 }}>Session</label>
        <select
          value={sessionName}
          onChange={(e) => selectSession(e.target.value)}
          disabled={busy || !sessionReady}
          style={{ flex: "1 1 200px", minWidth: 160 }}
        >
          {!sessionReady && <option value="">(starting…)</option>}
          {sessionReady && !sessions.some((s) => s.name === sessionName) && (
            <option value={sessionName}>{sessionName} (new)</option>
          )}
          {sessions.map((s) => (
            <option key={s.name} value={s.name}>
              {s.title || s.name}
            </option>
          ))}
        </select>
        <button
          disabled={busy || !host}
          onClick={newSession}
          title="Start a new session"
          style={{ fontSize: 11 }}
        >
          New session
        </button>
      </div>

      <div className="row" style={{ gap: 6, alignItems: "center", flexWrap: "wrap" }}>
        <button
          className={tab === "chat" ? "primary" : ""}
          onClick={() => setTab("chat")}
          style={{ fontSize: 11 }}
        >
          Chat
        </button>
        <button
          className={tab === "console" ? "primary" : ""}
          onClick={() => setTab("console")}
          style={{ fontSize: 11 }}
        >
          Console
        </button>
        <span style={{ flex: 1 }} />
        <label className="muted" style={{ fontSize: 11 }}>Model</label>
        <select
          value={model}
          onChange={(e) => setModel(e.target.value)}
          disabled={busy || noModels}
          style={{ flex: "1 1 200px", minWidth: 160 }}
        >
          {noModels && <option value="">(no models - install ollama)</option>}
          {models.map((m) => (
            <option key={m.name} value={m.name}>
              {m.name}
              {typeof m.size === "number" ? ` - ${humanSize(m.size)}` : ""}
            </option>
          ))}
        </select>
        <button
          disabled={busy}
          onClick={clear}
          title="Clear the transcript"
          style={{ fontSize: 11 }}
        >
          Clear
        </button>
      </div>

      {tab === "console" ? (
        <ConsolePanel host={host} client={client} logId={sessionName || "(starting…)"} />
      ) : (
        <>
          <div
            ref={threadRef}
            className="col"
            style={{
              gap: 8,
              flex: 1,
              minHeight: 120,
              maxHeight: "60vh",
              overflowY: "auto",
              background: "var(--bg-sunken)",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              padding: 8,
            }}
          >
            {transcript.length === 0 && (
              <div className="faint" style={{ padding: 10 }}>
                Ask anything. Every turn is one multi_inquire call — it decides on
                its own whether it can answer directly or needs to search first.
              </div>
            )}
            {transcript.map((t, i) => (
              <TurnBlock key={i} turn={t} host={host} onRun={onRun} />
            ))}
            {busy && (
              <div className="faint" style={{ fontSize: 11, padding: "2px 6px" }}>
                working...
              </div>
            )}
          </div>

          <Composer
            disabled={busy || !model || noModels || !sessionReady}
            running={busy}
            placeholder={
              !sessionReady
                ? "Starting a session…"
                : !model
                  ? "Pick a model first"
                  : noModels
                    ? "No models available - install solx-ollama"
                    : "Ask anything"
            }
            onSend={send}
            onStop={stop}
          />
        </>
      )}

      {error && (
        <div className="chip danger" style={{ alignSelf: "flex-start" }}>
          {error}
        </div>
      )}
    </div>
  );
}

function loadTranscript(): Turn[] {
  try {
    const raw = localStorage.getItem(LS_TRANSCRIPT);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as Turn[]) : [];
  } catch {
    return [];
  }
}

function humanSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "?";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let n = bytes;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i += 1;
  }
  return `${n.toFixed(n >= 10 ? 0 : 1)} ${units[i]}`;
}

function summarizeResult(hit: InquireHit, params: Record<string, unknown>): string {
  return `params: ${JSON.stringify(params)}\nmatch: ${hit.path}/${hit.name}`;
}

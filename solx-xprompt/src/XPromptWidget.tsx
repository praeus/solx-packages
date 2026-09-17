import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSolxWidgetClient } from "../../solx-widgets/src/wrap/SolxWidgetContext";
import { hostFromClient, type Host } from "../../solx-widgets/src/wrap/host";
import { dispatch } from "./dispatch";
import { LIST_MODELS, XPROMPT_SESSION_PATH } from "./refs";
import type { InquireHit, OllamaModel, Turn } from "./types";
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
const LS_SESSION_ID = "solx-xprompt.sessionId";

type Tab = "chat" | "console";

/**
 * ExecPrompt (XPrompt): turn a natural-language instruction into a
 * multi_inquire turn — a direct answer or a researched one, whichever the
 * instruction needs — with the merged console of that turn's own phases one
 * tab away.
 *
 * Every turn is tracked under one `sessionId`, generated once and kept in
 * localStorage alongside the transcript so a reload keeps the same Console
 * history. The transcript itself is still local-only, not a solx-inquiry
 * session document: multi_inquire's `session` param is passed a validly
 * shaped, per-widget ref so the call is well-formed, but nothing here saves
 * `session_document` back, so multi_inquire's own cross-turn history/memory
 * features are inert today — the widget's visible transcript is what
 * carries continuity for the user. See the README roadmap for persisting it.
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
  const [sessionId] = useState<string>(() => loadOrCreateSessionId());
  const [tab, setTab] = useState<Tab>("chat");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const sessionRef = useMemo(() => `${XPROMPT_SESSION_PATH}/${sessionId}`, [sessionId]);

  // Bumped on every send/stop. A new turn abandons any earlier one that
  // might still be settling — same pattern as solx-agent's genRef.
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
      if (!host || !client || !model) return;
      const myGen = ++genRef.current;
      const at = new Date().toISOString();
      setTranscript((prev) => [...prev, { kind: "user", text: message, at }]);
      setBusy(true);
      setError(null);
      void (async () => {
        try {
          const handle = await dispatch(client, host, sessionId, message, model, sessionRef);
          if (genRef.current !== myGen) return;
          invocationIdRef.current = handle.invocationId;
          const result = await handle.result;
          if (genRef.current !== myGen) return;
          setTranscript((prev) => [
            ...prev,
            { kind: "answer", model, result, at: new Date().toISOString() },
          ]);
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
    [host, client, model, sessionId, sessionRef],
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

  if (!client || !host) {
    return (
      <div className="muted" style={{ padding: 10 }}>
        No solx client - this widget needs a host that supplies one.
      </div>
    );
  }

  const noModels = models.length === 0;

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
        <ConsolePanel host={host} client={client} logId={sessionId} />
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
            disabled={busy || !model || noModels}
            running={busy}
            placeholder={
              !model
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

function loadOrCreateSessionId(): string {
  try {
    const existing = localStorage.getItem(LS_SESSION_ID);
    if (existing) return existing;
  } catch {
    /* localStorage is best-effort */
  }
  const id = newSessionId();
  try {
    localStorage.setItem(LS_SESSION_ID, id);
  } catch {
    /* best-effort */
  }
  return id;
}

function newSessionId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
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

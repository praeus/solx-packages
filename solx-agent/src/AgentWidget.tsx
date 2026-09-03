import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSolxWidgetClient } from "../../solx-widgets/src/wrap/SolxWidgetContext";
import {
  addTurn,
  approveAndContinue,
  createSession,
  driveSession,
  hostFromClient,
  isQuiescent,
  previewTools,
  readSession,
  summarize,
  widenGrant,
  LIST_MODELS,
  SEARCH_DOCS,
  SESSION_PATH,
  type AllowEntry,
  type Host,
  type OllamaModel,
  type Session,
  type SessionStatus,
  type StepResult,
} from "./harness";
import { buildTurnsFrom, type RenderedTurn } from "./transcript";
import { ApprovalCard } from "./components/ApprovalCard";
import { Composer } from "./components/Composer";
import { Header, type SessionSummary } from "./components/Header";
import { SetupPanel } from "./components/SetupPanel";
import { TurnBlock } from "./components/TurnBlock";
import {
  loadActiveSession,
  loadModel,
  loadSetup,
  saveActiveSession,
  saveModel,
  saveSetup,
  type SetupPrefs,
} from "./session/store";

export interface AgentWidgetFields {
  /** Optional heading. */
  title?: string;
  /** Open straight into an existing session instead of the last one used. */
  session_id?: string;
}

/**
 * A supervised agent thread.
 *
 * Chat-shaped, but not a chat: one message can mean a dozen iterations and
 * thirty tool calls against real documents, so the work is rendered rather
 * than hidden, and anything destructive stops for a decision.
 *
 * One session is one conversation is one document. The widget holds no
 * transcript of its own -- the session document under `/agent/sessions` is
 * the record -- so a reload, or another client, or `solx search`, sees the
 * same thread.
 *
 * The harness runs here too, in `src/harness/`. There is no backend half:
 * everything it does is an ordinary action call, which is all the wasm
 * component it replaced could do either.
 */
export function AgentWidget({ fields }: { fields: AgentWidgetFields | undefined }) {
  const client = useSolxWidgetClient();
  const host: Host | null = useMemo(
    () => (client ? hostFromClient(client) : null),
    [client],
  );

  const [setup, setSetup] = useState<SetupPrefs>(() => loadSetup());
  const [model, setModel] = useState<string>(() => loadModel());
  const [models, setModels] = useState<OllamaModel[]>([]);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);

  const [sessionId, setSessionId] = useState<string | null>(
    () => fields?.session_id ?? loadActiveSession(),
  );
  const [session, setSession] = useState<Session | null>(null);
  const [result, setResult] = useState<StepResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The live session object the loop mutates, kept out of React state so a
  // render never races the loop. `session` above is the snapshot to draw.
  const liveRef = useRef<Session | null>(null);
  // Set when the user opens a different session, so a loop still unwinding
  // stops publishing into a thread that is no longer on screen.
  const abandonRef = useRef(false);
  const threadRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => saveSetup(setup), [setup]);
  useEffect(() => saveModel(model), [model]);
  useEffect(() => saveActiveSession(sessionId), [sessionId]);

  const refreshSessions = useCallback(async (h: Host) => {
    try {
      const r = await h.try<{ hits?: SessionSummary[] }>(SEARCH_DOCS, {
        pathPrefix: SESSION_PATH,
        limit: 50,
      });
      if (r.ok && r.value?.hits) setSessions(r.value.hits);
    } catch {
      /* history is a convenience; a failure here is not worth a banner */
    }
  }, []);

  // Models and history, once the client is available.
  useEffect(() => {
    if (!host) return;
    let cancelled = false;
    void host
      .try<{ models?: OllamaModel[] }>(LIST_MODELS, {})
      .then((r) => {
        if (cancelled) return;
        // Only tool-capable models can drive this at all.
        const list = (r.ok && r.value?.models ? r.value.models : []).filter((m) =>
          (m.capabilities ?? []).includes("tools"),
        );
        setModels(list);
        setModel((current) => current || list[0]?.name || "");
      });
    void refreshSessions(host);
    return () => {
      cancelled = true;
    };
  }, [host, refreshSessions]);

  // Open whatever session we were pointed at.
  useEffect(() => {
    if (!host || !sessionId || session?.id === sessionId) return;
    let cancelled = false;
    void readSession(host, sessionId)
      .then((read) => {
        if (cancelled) return;
        liveRef.current = read;
        setSession(read);
        setResult(summarize(read, read.status));
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        // A session id from localStorage can outlive its document.
        setError(err instanceof Error ? err.message : String(err));
        setSessionId(null);
      });
    return () => {
      cancelled = true;
    };
  }, [host, sessionId, session?.id]);

  const turns: RenderedTurn[] = useMemo(
    () => (session ? buildTurnsFrom(session) : []),
    [session],
  );

  const status: SessionStatus | null = result?.status ?? session?.status ?? null;
  const running = busy || status === "running";

  // Follow the tail while work is coming in.
  useEffect(() => {
    if (threadRef.current) threadRef.current.scrollTop = threadRef.current.scrollHeight;
  }, [turns, status]);

  const handlers = useMemo(
    () => ({
      onProgress: (stepResult: StepResult, read: Session) => {
        if (abandonRef.current) return;
        setResult(stepResult);
        // A shallow copy per publish: the loop mutates the session in place,
        // and React needs a new reference to re-render.
        setSession({ ...read, messages: [...read.messages], calls: [...read.calls] });
      },
    }),
    [],
  );

  const run = useCallback(async (work: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (err) {
      if (!abandonRef.current) setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const send = useCallback(
    (message: string) => {
      if (!host) return;
      abandonRef.current = false;
      void run(async () => {
        const opts = { isAbandoned: () => abandonRef.current };
        let current = liveRef.current;
        let seed: StepResult;

        if (!current || current.id !== sessionId) {
          if (!model) throw new Error("Pick a model first.");
          if (setup.grant.length === 0) {
            throw new Error("Allow at least one action path — the gate is default-deny.");
          }
          current = await createSession(host, message, {
            model,
            grant: setup.grant,
            system: setup.system || null,
            catalogue_cap: setup.catalogueCap,
            tool_search: setup.toolSearch,
            max_iterations: setup.maxIterations,
            memory: setup.memoryScope ? { scope: setup.memoryScope } : null,
          });
          liveRef.current = current;
          setSessionId(current.id);
          seed = summarize(current, "running");
        } else {
          // The grant is the operator's to change mid-conversation; push any
          // edits made since the last turn before this one resolves its
          // catalogue against them.
          if (grantChanged(current.grant, setup.grant) && setup.grant.length > 0) {
            await widenGrant(host, current, setup.grant);
          }
          current.catalogue_cap = setup.catalogueCap;
          current.tool_search = setup.toolSearch;
          seed = await addTurn(host, current, message, {
            max_iterations: setup.maxIterations,
          });
        }

        await driveSession(host, current, seed, handlers, opts);
        void refreshSessions(host);
      });
    },
    [host, handlers, model, refreshSessions, run, sessionId, setup],
  );

  const decide = useCallback(
    (approve: string[]) => {
      const current = liveRef.current;
      if (!host || !current) return;
      void run(async () => {
        await approveAndContinue(host, current, approve, handlers, {
          isAbandoned: () => abandonRef.current,
        });
        void refreshSessions(host);
      });
    },
    [host, handlers, refreshSessions, run],
  );

  // Stop lands between iterations rather than mid-call: a tool that is
  // already running has already had its effect, and the loop persists after
  // each one, so the transcript stays true either way.
  const stop = useCallback(() => {
    abandonRef.current = true;
  }, []);

  const openSession = useCallback((id: string) => {
    abandonRef.current = true;
    liveRef.current = null;
    setError(null);
    setSession(null);
    setResult(null);
    setSessionId(id);
  }, []);

  const newSession = useCallback(() => {
    abandonRef.current = true;
    liveRef.current = null;
    setError(null);
    setSession(null);
    setResult(null);
    setSessionId(null);
  }, []);

  const onPreview = useCallback(
    (grant: AllowEntry[], query: string | null, cap: number) => {
      if (!host) return Promise.reject(new Error("no client"));
      return previewTools(host, grant, query, cap);
    },
    [host],
  );

  if (!client || !host) {
    return (
      <div className="muted" style={{ padding: 10 }}>
        No solx client — this widget needs a host that supplies one.
      </div>
    );
  }

  const pending = result?.pending ?? [];
  const awaitingApproval = status === "awaiting_approval" && pending.length > 0;

  return (
    <div className="col" style={{ gap: 8, height: "100%", minHeight: 320 }}>
      {fields?.title && <strong>{fields.title}</strong>}

      <Header
        models={models}
        model={model}
        onModel={setModel}
        status={status}
        iteration={result?.turn_iteration ?? session?.turn_iteration ?? 0}
        maxIterations={result?.max_iterations ?? setup.maxIterations}
        sessionId={sessionId}
        sessions={sessions}
        onOpenSession={openSession}
        onNewSession={newSession}
        busy={busy}
      />

      <SetupPanel
        setup={setup}
        onChange={setSetup}
        onPreview={onPreview}
        live={!!sessionId}
        queryHint={turns[turns.length - 1]?.user ?? ""}
      />

      <div
        ref={threadRef}
        className="col"
        style={{
          gap: 10,
          flex: 1,
          minHeight: 140,
          overflowY: "auto",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius)",
          padding: 8,
          background: "var(--bg-sunken)",
        }}
      >
        {turns.length === 0 && !busy && (
          <span className="faint">
            Describe what you want done. The agent will use the tools allowed above, and stop
            for your approval before anything destructive.
          </span>
        )}
        {turns.map((turn, i) => (
          <TurnBlock
            key={turn.key}
            turn={turn}
            live={running && i === turns.length - 1 && !awaitingApproval}
            defaultExpanded={i === turns.length - 1}
          />
        ))}
        {awaitingApproval && <ApprovalCard pending={pending} busy={busy} onDecide={decide} />}
        {status === "blocked" && (
          <span className="chip danger">
            Three iterations in a row failed — say something to redirect it.
          </span>
        )}
        {status === "exhausted" && (
          <span className="chip warn">
            Out of iterations for that turn — send another to continue.
          </span>
        )}
      </div>

      {error && <span className="chip danger">{error}</span>}

      <Composer
        disabled={busy || awaitingApproval}
        running={running}
        placeholder={
          sessionId
            ? status && isQuiescent(status)
              ? "Reply, or send it somewhere new…"
              : "Waiting…"
            : "What should the agent do?"
        }
        onSend={send}
        onStop={stop}
      />
    </div>
  );
}

/** Cheap structural compare: the grant is a handful of entries at most. */
function grantChanged(a: AllowEntry[], b: AllowEntry[]): boolean {
  return JSON.stringify(a) !== JSON.stringify(b);
}

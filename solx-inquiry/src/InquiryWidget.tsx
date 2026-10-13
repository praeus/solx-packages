import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSolxWidgetClient } from "../../solx-widgets/src/wrap/SolxWidgetContext";
import {
  applyOptions,
  buildCorpus,
  chat,
  cleanPlan,
  createInquiry,
  executePlan,
  hostFromClient,
  isQuiescent,
  loadInquiry,
  messageText,
  PLAN_SCHEMA,
  runInquiry,
  SEARCH_DOCS,
  SESSION_PATH,
  DELETE_DOC,
  LIST_MODELS,
  type Host,
  type InquirySession,
  type InquiryStatus,
  type OllamaModel,
  type StepResult,
} from "./harness";
import { PLANNER_BRIEF, plannerMessage } from "./harness/prompt";
import { buildTurns, type RenderedTurn } from "./transcript";
import { Header, type ModelsState, type InquirySummary } from "./components/Header";
import { Composer } from "./components/Composer";
import { OptionsPanel } from "./components/OptionsPanel";
import { TurnBlock } from "./components/TurnBlock";
import { PlanCard } from "./components/PlanCard";
import { DocumentsPane } from "./components/DocumentsPane";
import { loadActive, loadModel, loadSetup, saveActive, saveModel, saveSetup, type SetupPrefs } from "./session/store";

export interface InquiryWidgetFields {
  /** Optional heading. */
  title?: string;
  /** Open straight into an existing inquiry instead of the last one used. */
  session_id?: string;
}

/** A `StepResult`-shaped stand-in for a live plan-only preview. */
interface PreviewState {
  plan: ReturnType<typeof cleanPlan>["steps"];
  steps: Awaited<ReturnType<typeof executePlan>>["steps"];
  documents: Awaited<ReturnType<typeof executePlan>>["documents"];
}

/**
 * Ask a question, get an answer grounded in solx documents.
 *
 * Not a chat and not an agent: two model calls with a deterministic gather
 * between them, and a rendered record of what the model looked at. The widget
 * holds no transcript of its own -- the inquiry document under
 * `/inquiry/sessions` is the record -- so a reload, another client, or
 * `solx search` sees the same thread.
 *
 * The harness runs here too, in `src/harness/`. There is no backend half:
 * everything it does is an ordinary action call, which is all a widget can do.
 *
 * Deliberately smaller than solx-agent's widget: no approval card, no grant
 * panel, and a status set with no suspension in it. The one thing it has that
 * the agent does not is a *plan preview* -- the lookups are run and shown
 * before the answer is written, so a person can see the model understood the
 * question while it is still gathering.
 */
export function InquiryWidget({ fields }: { fields: InquiryWidgetFields | undefined }) {
  const client = useSolxWidgetClient();
  const host: Host | null = useMemo(() => (client ? hostFromClient(client) : null), [client]);

  const [setup, setSetup] = useState<SetupPrefs>(() => loadSetup());
  const [model, setModel] = useState<string>(() => loadModel());
  const [models, setModels] = useState<OllamaModel[]>([]);
  const [modelsState, setModelsState] = useState<ModelsState>("ok");
  const [inquiries, setInquiries] = useState<InquirySummary[]>([]);

  const [inquiryId, setInquiryId] = useState<string | null>(
    () => fields?.session_id ?? loadActive(),
  );
  const [session, setSession] = useState<InquirySession | null>(null);
  const [result, setResult] = useState<StepResult | null>(null);
  const [preview, setPreview] = useState<PreviewState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const liveRef = useRef<InquirySession | null>(null);
  // Bumped by every ask/stop/open/new. Each run captures the value current at
  // its start and compares against this on every check, so superseding it is
  // permanent -- a shared boolean could be un-abandoned by a later action and
  // publish into the wrong thread.
  const genRef = useRef(0);
  const threadRef = useRef<HTMLDivElement | null>(null);
  const [collapseSignal, setCollapseSignal] = useState(0);
  const stickToBottomRef = useRef(true);

  useEffect(() => saveSetup(setup), [setup]);
  useEffect(() => saveModel(model), [model]);
  useEffect(() => saveActive(inquiryId), [inquiryId]);

  const refreshInquiries = useCallback(async (h: Host) => {
    try {
      const r = await h.try<{ items?: InquirySummary[] }>(SEARCH_DOCS, {
        pathPrefix: SESSION_PATH,
        limit: 50,
      });
      if (r.ok && r.value?.items) setInquiries(r.value.items);
    } catch {
      /* history is a convenience; a failure here is not worth a banner */
    }
  }, []);

  useEffect(() => {
    if (!host) return;
    let cancelled = false;
    void host.try<{ models?: OllamaModel[] }>(LIST_MODELS, {}).then((r) => {
      if (cancelled) return;
      // `/api/tags` does not report capabilities, so only exclude a model when
      // capabilities are present and lack "tools". Inquiry's planner needs no
      // tools, but the answer call is still a chat call, and a model that
      // cannot chat is no use here.
      const all = r.ok && r.value?.models ? r.value.models : [];
      const list = all.filter((m) => !m.capabilities || m.capabilities.includes("tools"));
      setModelsState(!r.ok ? "unreachable" : list.length === 0 ? "none" : "ok");
      setModels(list);
      setModel((current) =>
        current && list.some((m) => m.name === current) ? current : list[0]?.name || "",
      );
    });
    void refreshInquiries(host);
    return () => {
      cancelled = true;
    };
  }, [host, refreshInquiries]);

  useEffect(() => {
    if (!host || !inquiryId || session?.id === inquiryId) return;
    let cancelled = false;
    void loadInquiry(host, inquiryId)
      .then((read) => {
        if (cancelled) return;
        liveRef.current = read;
        setSession(read);
        setResult(null);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
        setInquiryId(null);
      });
    return () => {
      cancelled = true;
    };
  }, [host, inquiryId, session?.id]);

  const turns: RenderedTurn[] = useMemo(
    () => (session ? buildTurns(session.turns) : []),
    [session],
  );

  const status: InquiryStatus | null = result?.status ?? session?.status ?? null;
  const running = busy || (status !== null && !isQuiescent(status));

  useEffect(() => {
    const el = threadRef.current;
    if (el && stickToBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [turns, status]);

  const onThreadScroll = useCallback(() => {
    const el = threadRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickToBottomRef.current = distanceFromBottom < 48;
  }, []);

  const makeHandlers = useCallback(
    (myGen: number) => ({
      onProgress: (stepResult: StepResult, read: InquirySession) => {
        if (genRef.current !== myGen) return;
        setPreview(null);
        setResult(stepResult);
        // A shallow copy per publish: the pipeline mutates the session in
        // place, and React needs a new reference to re-render.
        setSession({ ...read, turns: [...read.turns] });
      },
    }),
    [],
  );

  const run = useCallback(async (myGen: number, work: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (err) {
      if (genRef.current === myGen) setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (genRef.current === myGen) setBusy(false);
    }
  }, []);

  const ask = useCallback(
    (question: string) => {
      if (!host) return;
      const myGen = ++genRef.current;
      const handlers = makeHandlers(myGen);
      void run(myGen, async () => {
        const opts = { isAbandoned: () => genRef.current !== myGen };
        let current = liveRef.current;

        if (!current || current.id !== inquiryId) {
          if (!model) throw new Error("Pick a model first.");
          current = await createInquiry(host, question, {
            model,
            options: optionsFromSetup(setup),
          });
          if (genRef.current !== myGen) return;
          liveRef.current = current;
          setInquiryId(current.id);
        } else {
          applyOptions(current, optionsFromSetup(setup));
        }

        if (genRef.current !== myGen) return;
        await runInquiry(host, current, question, handlers, opts);
        if (genRef.current === myGen) void refreshInquiries(host);
      });
    },
    [host, inquiryId, makeHandlers, model, refreshInquiries, run, setup],
  );

  // A preview of the plan for the *next* question, without writing an answer.
  // This is the widget's one extra feature over the agent: it lets a person
  // see how the model reads a question before committing to an answer.
  const previewPlan = useCallback(
    (question: string) => {
      if (!host) return;
      const myGen = ++genRef.current;
      void run(myGen, async () => {
        const current =
          liveRef.current && liveRef.current.id === inquiryId
            ? liveRef.current
            : await createInquiry(host, question, { model, options: optionsFromSetup(setup) });
        applyOptions(current, optionsFromSetup(setup));
        if (genRef.current !== myGen) return;
        liveRef.current = current;
        setInquiryId(current.id);

        const corpus = await buildCorpus(host, setup.includeActions);
        const reply = await chat(
          host,
          {
            model: current.model,
            format: PLAN_SCHEMA,
            timeout_secs: setup.chatTimeoutSecs,
            messages: [
              { role: "system", content: PLANNER_BRIEF },
              { role: "user", content: plannerMessage(question, corpus.text, "") },
            ],
          },
          () => genRef.current !== myGen,
        );
        if (genRef.current !== myGen) return;
        let raw: unknown = { steps: [] };
        try {
          raw = JSON.parse(messageText(reply) || "{}");
        } catch {
          raw = { steps: [] };
        }
        const cleaned = cleanPlan(raw, setup.maxPlanSteps);
        const executed = await executePlan(host, cleaned.steps, setup.maxDocs);
        if (genRef.current !== myGen) return;
        setPreview({ plan: cleaned.steps, steps: executed.steps, documents: executed.documents });
      });
    },
    [host, inquiryId, model, run, setup],
  );

  const stop = useCallback(() => {
    genRef.current++;
    setBusy(false);
  }, []);

  const open = useCallback((id: string) => {
    genRef.current++;
    liveRef.current = null;
    stickToBottomRef.current = true;
    setError(null);
    setSession(null);
    setResult(null);
    setPreview(null);
    setInquiryId(id);
  }, []);

  const startNew = useCallback(() => {
    genRef.current++;
    liveRef.current = null;
    stickToBottomRef.current = true;
    setError(null);
    setSession(null);
    setResult(null);
    setPreview(null);
    setInquiryId(null);
  }, []);

  const remove = useCallback(async () => {
    if (!host || !inquiryId) return;
    const myGen = ++genRef.current;
    setBusy(true);
    setError(null);
    try {
      await host.call(DELETE_DOC, { path: SESSION_PATH, name: inquiryId });
    } catch (err) {
      if (genRef.current === myGen) setError(err instanceof Error ? err.message : String(err));
      return;
    } finally {
      if (genRef.current === myGen) setBusy(false);
    }
    if (genRef.current !== myGen) return;
    liveRef.current = null;
    setSession(null);
    setResult(null);
    setPreview(null);
    setInquiryId(null);
    void refreshInquiries(host);
  }, [host, inquiryId, refreshInquiries]);

  if (!client || !host) {
    return (
      <div className="muted" style={{ padding: 10 }}>
        No solx client — this widget needs a host that supplies one.
      </div>
    );
  }

  return (
    <div className="col" style={{ gap: 8, height: "100%", minHeight: 320 }}>
      {fields?.title && <strong>{fields.title}</strong>}

      <Header
        models={models}
        modelsState={modelsState}
        model={model}
        onModel={setModel}
        status={status}
        inquiryId={inquiryId}
        title={session?.title ?? null}
        inquiries={inquiries}
        onOpen={open}
        onNew={startNew}
        onDelete={remove}
        busy={busy}
      />

      <div
        ref={threadRef}
        onScroll={onThreadScroll}
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
        {turns.length === 0 && !busy && !preview && (
          <span className="faint">
            Ask a question about the documents in this solx instance. The model will decide
            what to look up, the lookups will run, and the answer will be written from what
            they found.
          </span>
        )}

        {turns.map((turn, i) => (
          <TurnBlock
            key={turn.key}
            turn={turn}
            live={running && i === turns.length - 1}
            defaultExpanded={i === turns.length - 1}
            collapseSignal={collapseSignal}
          />
        ))}

        {preview && (
          <div
            className="col"
            style={{
              gap: 6,
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              padding: 7,
              background: "var(--bg)",
            }}
          >
            <span className="muted" style={{ fontSize: 11 }}>
              plan preview — not yet answered
            </span>
            <PlanCard plan={preview.plan} steps={preview.steps} />
            {preview.documents.length > 0 && <DocumentsPane documents={preview.documents} />}
          </div>
        )}

        {status === "error" && result?.error && (
          <span className="chip danger">
            {result.error === "stopped" ? "Stopped." : "That did not finish: " + result.error}
          </span>
        )}
      </div>

      {error && <span className="chip danger">{error}</span>}

      <Composer
        disabled={busy}
        running={running}
        placeholder={
          inquiryId
            ? status && isQuiescent(status)
              ? "Ask a follow-up…"
              : "Waiting…"
            : "What do you want to know?"
        }
        onAsk={ask}
        onStop={stop}
      />

      <div className="row" style={{ gap: 8 }}>
        {turns.length > 0 && (
          <button
            onClick={() => setCollapseSignal((v) => v + 1)}
            className="muted"
            style={{ background: "none", border: "none", padding: 0, fontSize: 11 }}
          >
            Collapse all
          </button>
        )}
        <button
          onClick={() => {
            const last = turns[turns.length - 1];
            const q = last?.question;
            if (q) previewPlan(q);
          }}
          disabled={busy || turns.length === 0}
          className="muted"
          style={{ background: "none", border: "none", padding: 0, fontSize: 11 }}
          title="Re-run the lookups for the last question and show the plan, without writing an answer"
        >
          Preview last plan
        </button>
      </div>

      <OptionsPanel setup={setup} onChange={setSetup} live={!!inquiryId} />
    </div>
  );
}

function optionsFromSetup(setup: SetupPrefs) {
  return {
    max_docs: setup.maxDocs,
    max_plan_steps: setup.maxPlanSteps,
    doc_budget: setup.docBudget,
    include_actions: setup.includeActions,
    replan: setup.replan,
    chat_timeout_secs: setup.chatTimeoutSecs,
  };
}

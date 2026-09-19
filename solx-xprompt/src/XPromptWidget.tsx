import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSolxWidgetClient } from "../../solx-widgets/src/wrap/SolxWidgetContext";
import { hostFromClient, type Host } from "../../solx-widgets/src/wrap/host";
import {
  buildFollowUpInstruction,
  dispatch,
  runScript,
  unapprovedDestructiveRefs,
  type ForceKind,
  type StepOutcome,
} from "./dispatch";
import { LIST_MODELS, XPROMPT_SESSION_PATH } from "./refs";
import {
  deleteSession,
  listSessions,
  loadSessionTranscript,
  randomSessionName,
  saveSessionDocument,
} from "./session";
import type { InquireHit, MultiInquireResult, MultiInquireScript, OllamaModel, Turn, XPromptSessionSummary } from "./types";
import { Composer } from "./components/Composer";
import { TurnBlock } from "./components/TurnBlock";
import { ConsolePanel } from "./components/ConsolePanel";
import { ProgressStrip } from "./components/ProgressStrip";
import { ErrorBoundary, TurnRenderError } from "./components/ErrorBoundary";
import { SettingsPanel } from "./components/SettingsPanel";
import { useConsoleFeed } from "./console/useConsoleFeed";
import { foldProgress, type TurnOutcome } from "./progress";
import { normalizeTranscript } from "./result";

export interface XPromptWidgetFields {
  /** Optional heading. */
  title?: string;
  /** Open straight into a particular model; otherwise picks the first listed. */
  model?: string;
}

const LS_MODEL = "solx-xprompt.model";
const LS_TRANSCRIPT = "solx-xprompt.transcript";
const LS_SESSION_NAME = "solx-xprompt.sessionName";
const LS_AUTO_LOOP = "solx-xprompt.autoLoop";
const LS_AUTO_TURNS = "solx-xprompt.autoTurns";
const LS_FORCE_KIND = "solx-xprompt.forceKind";
/** Per-session allowlist of destructive action refs the user has approved. Keyed `solx-xprompt.approved.<sessionName>`. */
const LS_APPROVED_PREFIX = "solx-xprompt.approved.";

/** Cap on how many follow-up `next_prompt` turns the auto-loop will issue. */
const AUTO_TURNS_DEFAULT = 3;
export const AUTO_TURNS_MIN = 1;
export const AUTO_TURNS_MAX = 10;

/**
 * Clamp the turn cap wherever it comes from.
 *
 * This is the only bound on an autonomous loop that spends `1 + N` model
 * calls per turn, so it cannot be trusted to a control that happens to
 * enforce it: the Settings input clamped on change, but the localStorage
 * value was read back raw, and a hand-edited `999` was accepted whole.
 */
export function clampAutoTurns(n: number): number {
  if (!Number.isFinite(n)) return AUTO_TURNS_DEFAULT;
  return Math.min(AUTO_TURNS_MAX, Math.max(AUTO_TURNS_MIN, Math.trunc(n)));
}

type Tab = "chat" | "console" | "settings";

/**
 * One auto-loop follow-up: how many more are allowed after it, and the bare
 * `next_prompt` to show in the transcript — which is not what gets sent, since
 * the sent instruction also carries the previous turn's findings.
 */
interface LoopTurn {
  runsLeft: number;
  display: string;
}

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
  // How many exist, which is not how many are listed — see `SESSION_LIST_LIMIT`.
  const [sessionTotal, setSessionTotal] = useState(0);
  const [tab, setTab] = useState<Tab>("chat");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // How the last turn ended, as the widget itself saw it. The console feed
  // cannot be trusted for this: it is gated on `busy`, so the batch carrying
  // `run.done` races the turn's completion. See `ProgressStrip`.
  const [outcome, setOutcome] = useState<TurnOutcome | null>(null);
  // Per-session allowlist of destructive action refs the user has approved.
  // Cleared on session switch; persisted in localStorage under
  // `solx-xprompt.approved.<session>` so reloads keep approvals.
  const [approved, setApproved] = useState<ReadonlySet<string>>(() => loadApproved(sessionName));
  /**
   * Auto-loop: when a `multi_inquire` turn ends, if `intent.next_prompt` is
   * non-empty, automatically issue that as the next instruction (up to
   * `autoTurns` follow-ups per manual send). Default off — a loop without
   * an obvious off-switch is exactly the surprise we don't want. The
   * Stop button cancels a loop mid-flight via `genRef`.
   */
  const [autoLoop, setAutoLoop] = useState<boolean>(() => loadBool(LS_AUTO_LOOP, false));
  // Read at call time rather than through `dispatchNext`'s closure. A turn
  // already in flight captured whichever `dispatchNext` existed when it
  // started, so switching the loop off used to take one more turn to bite -
  // in a feature whose whole point is an obvious off-switch.
  const autoLoopRef = useRef(autoLoop);
  useEffect(() => {
    autoLoopRef.current = autoLoop;
  }, [autoLoop]);
  const [autoTurns, setAutoTurnsRaw] = useState<number>(() =>
    clampAutoTurns(loadInt(LS_AUTO_TURNS, AUTO_TURNS_DEFAULT)),
  );
  // Clamped on the way in as well as on the way out, so no caller can widen
  // the loop's bound by writing past the control that enforces it.
  const setAutoTurns = useCallback((n: number) => setAutoTurnsRaw(clampAutoTurns(n)), []);
  /**
   * Force the run into a specific inquiry shape without asking the model to
   * decide. `auto` is the default — the model's intent phase picks mode
   * and kind. `actions` synthesises a single action-inquiry so the scripts
   * schema is the one the model answers; `documents` does the same for
   * document reading. Persisted across sessions.
   */
  const [forceKind, setForceKindRaw] = useState<"auto" | ForceKind>(() =>
    loadForceKind(LS_FORCE_KIND),
  );
  const setForceKind = useCallback(
    (v: "auto" | ForceKind) => {
      setForceKindRaw(v);
      try {
        localStorage.setItem(LS_FORCE_KIND, v);
      } catch {
        /* best-effort */
      }
    },
    [],
  );

  const sessionRef = useMemo(() => `${XPROMPT_SESSION_PATH}/${sessionName}`, [sessionName]);

  // Bumped on every send/stop/session-switch. A new turn (or a switch away
  // from the session it belongs to) abandons any earlier one that might
  // still be settling — same pattern as solx-agent's genRef.
  const genRef = useRef(0);
  // The in-flight turn's invocation id, so Stop can cancel it for real via
  // client.invocations.stop rather than just abandoning the UI's wait.
  const invocationIdRef = useRef<string | null>(null);
  /**
   * Bumped whenever in-flight auto-run work should abandon: Stop, Clear, and
   * any session change. Deliberately *not* bumped by `send` — asking a
   * follow-up question while a plan runs is not a reason to halt the plan,
   * which is why this is separate from `genRef`.
   */
  const runGenRef = useRef(0);
  // The same id as reactive state. The ref does not re-render, and the
  // progress strip has to re-fold the moment the turn it belongs to changes.
  const [invocationId, setInvocationId] = useState<string | null>(null);
  const threadRef = useRef<HTMLDivElement | null>(null);

  // One tail loop for the whole widget, not one per panel: the Console tab
  // used to own it and so stopped reading the moment you switched to Chat.
  // Gated rather than always-on so an idle widget is not holding a long-poll
  // open forever - `busy` is true exactly when progress is worth watching.
  const feed = useConsoleFeed(host, client, sessionName, { active: busy || tab === "console" });
  // Folded over the whole retained buffer rather than incrementally, so the
  // state is a pure function of what has been seen: a reload re-reads the run
  // from its own `consoleSeqStart` and reconstructs exactly this.
  const progress = useMemo(
    () => foldProgress(feed.entries, invocationId),
    [feed.entries, invocationId],
  );

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

  useEffect(() => {
    try {
      localStorage.setItem(LS_AUTO_LOOP, autoLoop ? "1" : "0");
    } catch {
      /* best-effort */
    }
  }, [autoLoop]);

  useEffect(() => {
    try {
      localStorage.setItem(LS_AUTO_TURNS, String(autoTurns));
    } catch {
      /* best-effort */
    }
  }, [autoTurns]);

  // Switching sessions swaps the per-session destructive-allowlist. The
  // localStorage key is namespaced, so reloads pick up the same set.
  useEffect(() => {
    setApproved(loadApproved(sessionName));
  }, [sessionName]);

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
    void listSessions(host).then(({ sessions: list, total }) => {
      setSessions(list);
      setSessionTotal(total);
    });
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

  /**
   * Add one or more destructive action refs to the per-session allowlist.
   * Persisted to localStorage so a reload keeps the approval, scoped so a
   * fresh session starts clean.
   */
  const addApproved = useCallback(
    (refs: string[]) => {
      if (!refs.length) return;
      // The persisted write happens outside the updater (a state updater is
      // meant to be pure), but reads the updater's own result rather than
      // the `approved` closure: two calls can land in the same tick before
      // a re-render, and a closure-based read would then persist only the
      // second call's refs, dropping the first from localStorage.
      let persisted: ReadonlySet<string> = approved;
      setApproved((prev) => {
        const next = new Set(prev);
        for (const r of refs) next.add(r);
        persisted = next;
        return next;
      });
      saveApproved(sessionName, persisted);
    },
    [sessionName, approved],
  );

  /**
   * Drop every approved destructive ref for this session. The localStorage
   * entry is removed so a reload also starts fresh.
   */
  const clearApproved = useCallback(() => {
    setApproved(new Set());
    forgetApproved(sessionName);
  }, [sessionName]);

  /**
   * Push a single step outcome into the transcript as a run-turn.
   * Reuses the existing `kind: "run"` shape so `TurnBlock` renders it
   * unchanged; the `message` carries the step's index + status + result
   * summary.
   */
  const pushStepOutcome = useCallback((outcome: StepOutcome) => {
    const head = outcome.status === "ok"
      ? `step ${outcome.index + 1}: ${outcome.step.action_ref} -> ok`
      : outcome.status === "skipped"
        ? `step ${outcome.index + 1}: ${outcome.step.action_ref} -> skipped (${outcome.message})`
        : `step ${outcome.index + 1}: ${outcome.step.action_ref} -> error: ${outcome.message}`;
    const body = outcome.status === "ok" && outcome.result !== undefined
      ? summarizeGenericResult(outcome.result)
      : "";
    setTranscript((prev) => [
      ...prev,
      {
        kind: "run",
        status: outcome.status,
        message: body ? `${head}\n${body}` : head,
        at: new Date().toISOString(),
      },
    ]);
  }, []);

  /**
   * Bind one auto-run to the generation current when it starts.
   *
   * `shouldContinue` halts it between steps once that generation is stale,
   * and `onStep` drops late outcomes so a run abandoned by a session switch
   * cannot append to whatever transcript is open by the time it notices.
   */
  const bindRun = useCallback(() => {
    const myGen = runGenRef.current;
    return {
      shouldContinue: () => runGenRef.current === myGen,
      onStep: (outcome: StepOutcome) => {
        if (runGenRef.current !== myGen) return;
        pushStepOutcome(outcome);
      },
      /** Surface a thrown run rather than losing it to an unhandled rejection. */
      onThrow: (err: unknown) => {
        if (runGenRef.current !== myGen) return;
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        setTranscript((prev) => [
          ...prev,
          { kind: "error", message: `auto-run failed: ${message}`, at: new Date().toISOString() },
        ]);
      },
    };
  }, [pushStepOutcome]);

  /**
   * Ask once about every destructive ref a run needs and does not yet have.
   *
   * Returns the set to run under, or `null` if the user declined. The grant
   * is durable — written to localStorage under this session's key, so it
   * survives a reload — and it is per *action*, not per call: the wording
   * says both, because neither is obvious from a button labelled "Run plan".
   */
  const approveIfNeeded = useCallback(
    (unapproved: string[], what: string): ReadonlySet<string> | null => {
      if (unapproved.length === 0) return approved;
      const ok = window.confirm(
        `${what} ${unapproved.length} destructive action(s):\n\n` +
          unapproved.map((r) => "  • " + r).join("\n") +
          `\n\nApprove these for this session? They will then run without asking again, ` +
          `with whatever parameters a later plan gives them, until you clear approvals in Settings.`,
      );
      if (!ok) return null;
      addApproved(unapproved);
      // `setApproved` is async, so this run gets the merged set explicitly
      // rather than reading a state value that has not updated yet.
      const merged = new Set(approved);
      for (const r of unapproved) merged.add(r);
      return merged;
    },
    [approved, addApproved],
  );

  /**
   * Run an entire `multi_inquire` plan in order, asking first about any
   * destructive step not yet approved.
   */
  const onRunScript = useCallback(
    (script: MultiInquireScript) => {
      if (!host) return;
      const effective = approveIfNeeded(
        unapprovedDestructiveRefs(script, approved),
        "This plan contains",
      );
      if (!effective) return;
      const { shouldContinue, onStep, onThrow } = bindRun();
      void runScript(host, script, { approved: effective, onStep, shouldContinue }).catch(onThrow);
    },
    [host, approved, approveIfNeeded, bindRun],
  );

  /**
   * Decide whether to follow `result.intent.next_prompt` with another
   * `multi_inquire` call. Bound by `runsLeft` so a runaway prompt never
   * recurses forever. Driven by `autoLoop` so the user can flip it off
   * mid-session. The reference to `send` is via `sendRef` because the two
   * callbacks cross-reference and a direct dependency would create a
   * circular `useCallback` initialiser.
   */
  const sendRef = useRef<(message: string, loop?: LoopTurn) => void>(() => {});
  const dispatchNext = useCallback(
    (last: MultiInquireResult, runsLeft: number) => {
      if (!autoLoopRef.current) return;
      if (runsLeft <= 0) return;
      const next = last.intent?.next_prompt;
      if (typeof next !== "string" || next.trim() === "") return;
      // setBusy(true) right before the recurse so the UI stays "working"
      // through the gap between turns; the follow-up flips it off in its own
      // finally and the strip never goes blank.
      setBusy(true);
      // What gets *sent* carries the previous turn's findings, because
      // `next_prompt` was written before that turn searched anything (see
      // `buildFollowUpInstruction`). What gets *shown* is the bare
      // suggestion - the findings are already on screen in the answer turn
      // above it, and repeating them in the transcript would bury it.
      sendRef.current(buildFollowUpInstruction(next, last), {
        runsLeft: runsLeft - 1,
        display: next,
      });
    },
    [],
  );

  const send = useCallback(
    (message: string, loop?: LoopTurn) => {
      if (!host || !client || !model || !sessionName) return;
      const myGen = ++genRef.current;
      const at = new Date().toISOString();
      // The user-typed instruction is the only one rendered as a user-turn;
      // follow-ups driven by `next_prompt` skip the user bubble so the
      // transcript reads as "model continued" rather than "user said".
      if (!loop) {
        setTranscript((prev) => [...prev, { kind: "user", text: message, at }]);
      } else {
        setTranscript((prev) => [
          ...prev,
          { kind: "run", status: "ok", message: `continue: ${loop.display}`, at },
        ]);
      }
      setBusy(true);
      setError(null);
      setOutcome(null);
      void (async () => {
        try {
          const handle = await dispatch(
            client,
            host,
            sessionName,
            message,
            model,
            sessionRef,
            { forceKind: forceKind === "auto" ? undefined : forceKind },
          );
          if (genRef.current !== myGen) return;
          invocationIdRef.current = handle.invocationId;
          setInvocationId(handle.invocationId);
          const result = await handle.result;
          if (genRef.current !== myGen) return;
          setTranscript((prev) => [
            ...prev,
            { kind: "answer", model, result, at: new Date().toISOString() },
          ]);
          // Never saved by multi_inquire itself — persisting it here is what
          // gives the *next* turn's intent phase this one's history to read.
          let saved = true;
          if (result.session_document) {
            try {
              await saveSessionDocument(host, result.session_document);
              refreshSessions();
            } catch (err) {
              // The turn itself still succeeded — a failed save shouldn't
              // turn a good answer into an error turn, just get surfaced.
              saved = false;
              if (genRef.current === myGen) {
                setError("Session not saved: " + (err instanceof Error ? err.message : String(err)));
              }
            }
          }
          // Auto-loop: if `intent.next_prompt` is set AND the loop is on,
          // recurse into another turn. The loop is bounded by `autoTurns`
          // on the first call, and decremented on each follow-up.
          //
          // Not after a failed save: the session document is the only thing
          // that carries this turn into the next one's history, so a
          // follow-up issued now would run as if this turn had never
          // happened - looping on a suggestion while blind to what produced
          // it. Better to stop with the error on screen.
          if (genRef.current === myGen) setOutcome("done");
          if (genRef.current === myGen && saved) {
            dispatchNext(result, loop ? loop.runsLeft : autoTurns);
          }
        } catch (err) {
          if (genRef.current !== myGen) return;
          setOutcome("error");
          setError(err instanceof Error ? err.message : String(err));
          setTranscript((prev) => [
            ...prev,
            { kind: "error", message: err instanceof Error ? err.message : String(err), at: new Date().toISOString() },
          ]);
        } finally {
          if (genRef.current === myGen) {
            // The auto-loop fires *after* the answer is on screen, so the
            // transcript stays in sync turn-by-turn. We set `busy` back to
            // true inside `dispatchNext` to keep the spinner honest.
            setBusy(false);
            invocationIdRef.current = null;
            // Deliberately not clearing `invocationId`: the strip keeps
            // showing how the finished turn went until the next one starts.
          }
        }
      })();
    },
    [host, client, model, sessionName, sessionRef, refreshSessions, dispatchNext, autoTurns, forceKind],
  );

  // Keep `sendRef` in lockstep with the latest `send` so the auto-loop hook
  // (which lives above `send` in source order) can call back into it
  // without a stale closure.
  useEffect(() => {
    sendRef.current = send;
  }, [send]);

  const stop = useCallback(() => {
    genRef.current++;
    // Stop means stop: without this the multi_inquire invocation was
    // cancelled but an executing plan ran on to its last step.
    runGenRef.current++;
    setBusy(false);
    setOutcome("stopped");
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
    runGenRef.current++;
    setTranscript([]);
    setError(null);
    setInvocationId(null);
    setOutcome(null);
  }, []);

  // Start a new, unrelated session: a fresh name, an empty transcript. The
  // old session's document (if it ever got one) is untouched — switching
  // away doesn't delete or rename anything.
  const newSession = useCallback(() => {
    if (!host) return;
    genRef.current++;
    runGenRef.current++;
    setBusy(false);
    invocationIdRef.current = null;
    setInvocationId(null);
    setOutcome(null);
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

  /**
   * Delete a session and everything filed under its name.
   *
   * Confirms first, and says what deletion does and does not cover:
   * `entity-delete-document` is an `internal` action with no capabilities, so
   * nothing in solx-core will stop for it — this dialog is the only gate. And
   * the run's console output cannot be removed at all, because console entries
   * are keyed by `action_ref` and shared with every other session.
   *
   * Deleting the session you are *on* falls through to `newSession()`, so the
   * widget is never left pointing at something that no longer exists.
   */
  const removeSession = useCallback(
    (name: string) => {
      if (!host || !name) return;
      const isCurrent = name === sessionName;
      const ok = window.confirm(
        `Delete session "${name}"?\n\n` +
          `This removes its saved history and its call log permanently, and ` +
          `forgets any destructive actions approved under it.\n\n` +
          `Console output is not removed — it is shared with every other ` +
          `session and ages out on its own.` +
          (isCurrent ? "\n\nThis is the session you are on; a new one will be started." : ""),
      );
      if (!ok) return;
      void (async () => {
        try {
          await deleteSession(host, name);
        } catch (err) {
          setError("Session not deleted: " + (err instanceof Error ? err.message : String(err)));
          return;
        }
        forgetApproved(name);
        if (isCurrent) {
          newSession();
        } else {
          refreshSessions();
        }
      })();
    },
    [host, sessionName, newSession, refreshSessions],
  );

  // Switch to an existing session from the picker: abandon whatever this
  // widget instance was doing, load that session's saved turns back as a
  // transcript (see loadSessionTranscript's caveats — no hits, no per-turn
  // model/timestamp), and make it the one new turns are sent under.
  const selectSession = useCallback(
    (name: string) => {
      if (!host || !name || name === sessionName) return;
      genRef.current++;
      runGenRef.current++;
      setBusy(false);
      invocationIdRef.current = null;
      setInvocationId(null);
      setOutcome(null);
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
          ExecPrompt - ask anything, the model decides whether to look things up
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
        <button
          disabled={busy || !host || !sessionReady}
          onClick={() => removeSession(sessionName)}
          title="Delete this session and its saved history"
          style={{ fontSize: 11 }}
        >
          {/* Asks first, like every other irreversible affordance here. */}
          Delete…
        </button>
      </div>

      <div
        className="row"
        style={{ gap: 6, alignItems: "center", flexWrap: "wrap" }}
      >
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
        <label className="muted" style={{ fontSize: 11 }}>Mode</label>
        <select
          value={forceKind}
          onChange={(e) => setForceKind(e.target.value as "auto" | ForceKind)}
          disabled={busy}
          title="Auto: the model's intent phase decides. Actions / Documents: skip the intent phase and route the inquiry through one of those."
          style={{ flex: "0 0 auto", minWidth: 110 }}
        >
          <option value="auto">Auto</option>
          <option value="actions">Actions</option>
          <option value="documents">Documents</option>
        </select>
      </div>

      <div className="row" style={{ gap: 6, alignItems: "center" }}>
        <button
          className={tab === "chat" ? "primary" : ""}
          onClick={() => setTab("chat")}
          style={{ fontSize: 11, flex: 1 }}
        >
          Chat
        </button>
        <button
          className={tab === "console" ? "primary" : ""}
          onClick={() => setTab("console")}
          style={{ fontSize: 11, flex: 1 }}
        >
          Console
        </button>
        <button
          className={tab === "settings" ? "primary" : ""}
          onClick={() => setTab("settings")}
          style={{ fontSize: 11, flex: 1 }}
        >
          Settings
        </button>
      </div>

      {/* Above the tab body, so the same strip is visible on every tab. */}
      <ProgressStrip progress={progress} busy={busy} outcome={outcome} />

      {tab === "console" && (
        <ConsolePanel entries={feed.entries} error={feed.error} />
      )}

      {tab === "chat" && (
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
                Ask anything. Every turn asks the model once — it decides on
                its own whether it can answer directly or needs to search first.
              </div>
            )}
            {/* Per turn, not just at the root: a turn that cannot be
                rendered becomes one error card instead of blanking a widget
                whose transcript then survives the reload. */}
            {transcript.map((t, i) => (
              <ErrorBoundary
                key={i}
                label={`turn ${i + 1}`}
                fallback={(err) => <TurnRenderError error={err} />}
              >
                <TurnBlock
                  turn={t}
                  host={host}
                  onRun={onRun}
                  onRunScript={onRunScript}
                  approved={approved}
                />
              </ErrorBoundary>
            ))}
            {/* The former `working...` line lived here. The progress strip
                above the tab body replaces it, and says what is happening
                rather than only that something is. */}
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

      {tab === "settings" && (
        <SettingsPanel
          autoLoop={autoLoop}
          setAutoLoop={setAutoLoop}
          autoTurns={autoTurns}
          setAutoTurns={setAutoTurns}
          approvedCount={approved.size}
          onClearApprovals={clearApproved}
          sessions={sessions}
          sessionTotal={sessionTotal}
          currentSession={sessionName}
          onDeleteSession={removeSession}
          busy={busy}
          onClearTranscript={clear}
        />
      )}

      {error && (
        <div className="chip danger" style={{ alignSelf: "flex-start" }}>
          {error}
        </div>
      )}
    </div>
  );
}

/**
 * The persisted transcript, repaired.
 *
 * This is the one input nobody validates: it was written by whichever build
 * of the widget ran last, it survives reloads, and it feeds straight into
 * `TurnBlock`. `normalizeTranscript` fills what it can and drops what it
 * cannot, so an entry written by an older shape costs that entry rather than
 * the session.
 */
function loadTranscript(): Turn[] {
  try {
    const raw = localStorage.getItem(LS_TRANSCRIPT);
    if (!raw) return [];
    return normalizeTranscript(JSON.parse(raw));
  } catch {
    return [];
  }
}

function loadBool(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    return raw === "1" || raw === "true";
  } catch {
    return fallback;
  }
}

function loadInt(key: string, fallback: number): number {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    const n = parseInt(raw, 10);
    return Number.isFinite(n) ? n : fallback;
  } catch {
    return fallback;
  }
}

/**
 * Load the user's saved inquiry-mode override. Anything other than the
 * three known values falls back to `"auto"` so a bad localStorage value
 * (or a future removed value) silently disables the override rather than
 * corrupting state.
 */
function loadForceKind(key: string): "auto" | ForceKind {
  try {
    const raw = localStorage.getItem(key);
    if (raw === "actions" || raw === "documents" || raw === "auto") return raw;
  } catch {
    /* best-effort */
  }
  return "auto";
}

/** Drop a session's destructive allowlist. Safe for a session that has none. */
function forgetApproved(session: string): void {
  if (!session) return;
  try {
    localStorage.removeItem(LS_APPROVED_PREFIX + session);
  } catch {
    /* best-effort */
  }
}

/** Load the destructive-allowlist for a given session (or empty). */
function loadApproved(session: string): ReadonlySet<string> {
  if (!session) return new Set();
  try {
    const raw = localStorage.getItem(LS_APPROVED_PREFIX + session);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return new Set(parsed.filter((x) => typeof x === "string"));
    return new Set();
  } catch {
    return new Set();
  }
}

function saveApproved(session: string, set: ReadonlySet<string>): void {
  if (!session) return;
  try {
    localStorage.setItem(LS_APPROVED_PREFIX + session, JSON.stringify(Array.from(set)));
  } catch {
    /* best-effort */
  }
}

/**
 * Format an arbitrary action result for a run-turn message. The shape is
 * action-defined — we surface only what reads well in one or two lines.
 */
function summarizeGenericResult(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value.length > 240 ? value.slice(0, 240) + "…" : value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  try {
    const s = JSON.stringify(value);
    return s.length > 240 ? s.slice(0, 240) + "…" : s;
  } catch {
    return "(unserialisable result)";
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

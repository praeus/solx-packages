import { describe, expect, it } from "vitest";
import type { MergedEntry } from "../src/console";
import { legacyEventFromMessage, parseProgressEvent } from "../src/progress/events";
import type { ProgressEvent } from "../src/progress/events";
import { emptyProgress, foldProgress, reduceProgress } from "../src/progress/reducer";
import type { ProgressState } from "../src/progress/reducer";
import { inquiryChipLabel, inquiryChipClass, inquiryTooltip, phaseWord, stageLabel } from "../src/progress/labels";

const REF = "/packages/solx-inquiry/multi-inquire";
const INV = "inv-1";

function entry(over: Partial<MergedEntry> = {}): MergedEntry {
  return {
    seq: 1,
    ts: "2026-01-01T00:00:00.000Z",
    level: "info",
    actionRef: REF,
    invocationId: INV,
    label: "turn",
    message: "a line with no tag",
    data: null,
    ...over,
  };
}

/** One entry carrying a progress envelope, as `solx-inquiry` prints it. */
function withEvent(ev: Record<string, unknown>, over: Partial<MergedEntry> = {}): MergedEntry {
  return entry({ data: { ev: { v: 1, ...ev } }, ...over });
}

describe("parsing progress events", () => {
  it("reads a well-formed envelope into a typed event", () => {
    const e = parseProgressEvent(
      withEvent({ t: "inquiry.started", i: 2, n: 3, kind: "documents", q: "what is auth?" }),
    );
    expect(e).toEqual({ t: "inquiry.started", i: 2, n: 3, kind: "documents", q: "what is auth?" });
  });

  it("keeps the domain half of data out of the event", () => {
    // The envelope sits beside the existing payload, never replacing it.
    const e = parseProgressEvent(
      entry({ data: { count: 7, refs: ["/notes/auth"], ev: { v: 1, t: "inquiry.searched", i: 0, hits: 7 } } }),
    );
    expect(e).toEqual({ t: "inquiry.searched", i: 0, hits: 7 });
  });

  it("is not an event when there is no data at all", () => {
    expect(parseProgressEvent(entry())).toBeNull();
  });

  it("is not an event when data carries no envelope and no legacy tag", () => {
    expect(parseProgressEvent(entry({ data: { count: 7 } }))).toBeNull();
  });

  it("drops an unknown event type rather than throwing", () => {
    expect(parseProgressEvent(withEvent({ t: "something.new", i: 0 }))).toBeNull();
  });

  it("drops a newer envelope version rather than guessing at it", () => {
    expect(parseProgressEvent(entry({ data: { ev: { v: 2, t: "inquiry.started", i: 0 } } }))).toBeNull();
  });

  it("drops an inquiry event whose index is missing or malformed", () => {
    expect(parseProgressEvent(withEvent({ t: "inquiry.started" }))).toBeNull();
    expect(parseProgressEvent(withEvent({ t: "inquiry.started", i: "2" }))).toBeNull();
    expect(parseProgressEvent(withEvent({ t: "inquiry.started", i: -1 }))).toBeNull();
    expect(parseProgressEvent(withEvent({ t: "inquiry.started", i: 1.5 }))).toBeNull();
  });

  it("drops an intent.done with no usable mode", () => {
    expect(parseProgressEvent(withEvent({ t: "intent.done", n: 2 }))).toBeNull();
    expect(parseProgressEvent(withEvent({ t: "intent.done", mode: "sideways" }))).toBeNull();
  });

  it("treats a missing ok as not-ok rather than as unknown", () => {
    expect(parseProgressEvent(withEvent({ t: "inquiry.finished", i: 0 }))).toEqual({
      t: "inquiry.finished",
      i: 0,
      status: undefined,
      ok: false,
    });
  });

  it("never throws on junk", () => {
    for (const data of [0, "x", [], [1, 2], { ev: 3 }, { ev: [] }, { ev: null }]) {
      expect(() => parseProgressEvent(entry({ data }))).not.toThrow();
    }
  });
});

describe("the legacy tag shim", () => {
  // A widget bundle updated ahead of the .wasm package is the normal state,
  // not an edge case - without this the strip would simply stay empty.
  it("recovers a searched inquiry from a pre-envelope hits print", () => {
    const e = legacyEventFromMessage(
      entry({ message: "[multi_inquire:inquiry:0:hits] 7 hit(s)", data: { count: 7 } }),
    );
    expect(e).toEqual({ t: "inquiry.searched", i: 0, hits: 7 });
  });

  it("recovers the planned question and scope from a terms print", () => {
    const e = legacyEventFromMessage(
      entry({
        message: "[multi_inquire:inquiry:1:terms] what is auth?",
        data: { kind: "documents", question: "what is auth?" },
      }),
    );
    expect(e).toEqual({ t: "inquiry.planned", i: 1, kind: "documents", q: "what is auth?" });
  });

  it("treats a pre-envelope result print as the only evidence of completion", () => {
    const e = legacyEventFromMessage(entry({ message: "[multi_inquire:inquiry:2:result] 1 response(s)" }));
    expect(e).toEqual({ t: "inquiry.finished", i: 2, status: "ok", ok: true });
  });

  it("is reached through the normal parser when no envelope is present", () => {
    const e = parseProgressEvent(
      entry({ message: "[multi_inquire:inquiry:0:hits] 7 hit(s)", data: { count: 7 } }),
    );
    expect(e).toEqual({ t: "inquiry.searched", i: 0, hits: 7 });
  });

  it("is not reached when an envelope is present", () => {
    // The envelope wins even where the message would also have parsed.
    const e = parseProgressEvent(
      entry({
        message: "[multi_inquire:inquiry:0:hits] 7 hit(s)",
        data: { count: 7, ev: { v: 1, t: "inquiry.searched", i: 0, hits: 99 } },
      }),
    );
    expect(e).toEqual({ t: "inquiry.searched", i: 0, hits: 99 });
  });

  it("ignores a message with no multi_inquire tag", () => {
    expect(legacyEventFromMessage(entry({ message: "just some output" }))).toBeNull();
    expect(legacyEventFromMessage(entry({ message: null }))).toBeNull();
  });
});

/** Fold a scripted event sequence, as if every one had been read in order. */
function fold(events: ProgressEvent[], invocationId = INV): ProgressState {
  return events.reduce((s, e) => reduceProgress(s, e, invocationId), emptyProgress());
}

const RESEARCHED: ProgressEvent[] = [
  { t: "run.started", n: 3, model: "qwen3:4b" },
  { t: "recall.done", skills: 1, memories: 0 },
  { t: "context.done", docs: 0 },
  { t: "intent.started", model: "qwen3:4b" },
  { t: "intent.done", mode: "inquire", n: 3 },
  { t: "inquiry.planned", i: 0, n: 3, kind: "documents", q: "what is auth?" },
  { t: "inquiry.searched", i: 0, hits: 7 },
  { t: "inquiry.planned", i: 1, n: 3, kind: "documents", q: "what are sessions?" },
  { t: "inquiry.searched", i: 1, hits: 2 },
  { t: "inquiry.planned", i: 2, n: 3, kind: "actions", q: "how do I search?" },
  { t: "inquiry.searched", i: 2, hits: 4 },
  { t: "inquiry.started", i: 0, n: 3, kind: "documents", q: "what is auth?" },
  { t: "inquiry.started", i: 1, n: 3, kind: "documents", q: "what are sessions?" },
  { t: "inquiry.started", i: 2, n: 3, kind: "actions", q: "how do I search?" },
  { t: "fanout.started", n: 3, mode: "parallel" },
];

describe("folding events into progress", () => {
  it("walks a researched run through its phases", () => {
    const mid = fold(RESEARCHED);
    expect(mid.stage).toBe("fanout");
    expect(mid.mode).toBe("inquire");
    expect(mid.expected).toBe(3);
    expect(mid.inquiries.map((one) => one.phase)).toEqual(["running", "running", "running"]);
    expect(stageLabel(mid)).toBe("3 inquiries running");

    const done = fold([
      ...RESEARCHED,
      { t: "inquiry.finished", i: 1, status: "ok", ok: true },
      { t: "inquiry.finished", i: 0, status: "ok", ok: true },
      { t: "inquiry.finished", i: 2, status: "ok", ok: true },
      { t: "run.done", responses: 2, scripts: 1, memories: 0, errors: 0 },
    ]);
    expect(done.stage).toBe("done");
    expect(done.inquiries.map((one) => one.phase)).toEqual(["done", "done", "done"]);
  });

  it("counts only the inquiries still running", () => {
    const s = fold([...RESEARCHED, { t: "inquiry.finished", i: 0, status: "ok", ok: true }]);
    expect(stageLabel(s)).toBe("2 inquiries running");
  });

  it("says one inquiry, not 1 inquiries", () => {
    const s = fold([
      { t: "intent.done", mode: "inquire", n: 1 },
      { t: "inquiry.started", i: 0, n: 1, q: "a" },
    ]);
    expect(stageLabel(s)).toBe("1 inquiry running");
  });

  it("keeps an inquiry it hears about out of order", () => {
    // A lost `inquiry.started` must cost the question text, not the row.
    const s = fold([
      { t: "intent.done", mode: "inquire", n: 2 },
      { t: "inquiry.finished", i: 1, status: "ok", ok: true },
    ]);
    expect(s.inquiries).toHaveLength(1);
    expect(s.inquiries[0]).toMatchObject({ index: 1, phase: "done" });
    expect(s.inquiries[0].question).toBeUndefined();
  });

  it("never moves an inquiry backwards", () => {
    const s = fold([
      { t: "inquiry.started", i: 0, q: "a" },
      { t: "inquiry.finished", i: 0, status: "ok", ok: true },
      // A late duplicate of an earlier phase must not undo the completion.
      { t: "inquiry.planned", i: 0, q: "a" },
      { t: "inquiry.searched", i: 0, hits: 3 },
    ]);
    expect(s.inquiries[0].phase).toBe("done");
    // Detail still accumulates - only the phase is one-way.
    expect(s.inquiries[0].hits).toBe(3);
  });

  it("is idempotent under duplicate delivery", () => {
    const once = fold(RESEARCHED);
    const twice = fold([...RESEARCHED, ...RESEARCHED]);
    expect(twice.inquiries).toEqual(once.inquiries);
    expect(twice.expected).toBe(once.expected);
    expect(twice.stage).toBe(once.stage);
  });

  it("lets failed win over done in either arrival order", () => {
    // One failure is reported twice on purpose - the lifecycle edge and the
    // diagnosis - and they can arrive either way round.
    const a = fold([
      { t: "inquiry.finished", i: 0, status: "failed", ok: false },
      { t: "inquiry.failed", i: 0, reason: "llm_error" },
    ]);
    const b = fold([
      { t: "inquiry.failed", i: 0, reason: "llm_error" },
      { t: "inquiry.finished", i: 0, status: "failed", ok: false },
    ]);
    expect(a.inquiries[0].phase).toBe("failed");
    expect(b.inquiries[0].phase).toBe("failed");
    expect(a.inquiries[0].reason).toBe("llm_error");
    expect(b.inquiries[0].reason).toBe("llm_error");
  });

  it("does not let a stray done undo a failure", () => {
    const s = fold([
      { t: "inquiry.failed", i: 0, reason: "llm_error" },
      { t: "inquiry.finished", i: 0, status: "ok", ok: true },
    ]);
    expect(s.inquiries[0].phase).toBe("failed");
  });

  it("never shrinks the expected count", () => {
    const s = fold([
      { t: "intent.done", mode: "inquire", n: 3 },
      { t: "fanout.started", n: 1, mode: "parallel" },
    ]);
    expect(s.expected).toBe(3);
  });

  it("opens no fan-out row for a direct answer", () => {
    const s = fold([
      { t: "run.started", n: 3 },
      { t: "intent.started" },
      { t: "intent.done", mode: "direct", n: 0 },
      { t: "run.done", responses: 1 },
    ]);
    expect(s.mode).toBe("direct");
    expect(s.expected).toBe(0);
    expect(s.inquiries).toEqual([]);
    expect(s.stage).toBe("done");
  });

  it("does not size the strip from the run's cap", () => {
    // `run.started.n` is `max_inquiries`, not a count of anything proposed.
    const s = fold([{ t: "run.started", n: 3 }]);
    expect(s.expected).toBeUndefined();
    expect(s.inquiries).toEqual([]);
  });

  it("records a cancelled run as cancelled, not as still working", () => {
    const s = fold([...RESEARCHED, { t: "run.cancelled", stopped: 3 }]);
    expect(s.stage).toBe("cancelled");
    expect(stageLabel(s)).toBe("cancelled");
  });

  it("records a degraded fan-out and why", () => {
    const s = fold([
      { t: "intent.done", mode: "inquire", n: 3 },
      { t: "fanout.degraded", n: 3, mode: "sequential", reason: "host_cannot_detach" },
    ]);
    expect(s.degraded).toBe("host_cannot_detach");
  });

  it("does not leave a settled stage", () => {
    const s = fold([
      { t: "intent.done", mode: "inquire", n: 1 },
      { t: "run.done", responses: 0 },
      { t: "inquiry.started", i: 0, q: "late" },
    ]);
    expect(s.stage).toBe("done");
  });

  it("starts over when a different invocation reports", () => {
    let s = fold(RESEARCHED);
    expect(s.inquiries).toHaveLength(3);
    s = reduceProgress(s, { t: "run.started", n: 3 }, "inv-2");
    expect(s.invocationId).toBe("inv-2");
    expect(s.inquiries).toEqual([]);
    expect(s.stage).toBe("recall");
  });
});

describe("folding a console feed", () => {
  const entries: MergedEntry[] = [
    withEvent({ t: "run.started", n: 3 }, { seq: 1 }),
    withEvent({ t: "intent.done", mode: "inquire", n: 2 }, { seq: 2 }),
    withEvent({ t: "inquiry.started", i: 0, n: 2, kind: "documents", q: "a" }, { seq: 3 }),
    withEvent({ t: "inquiry.started", i: 1, n: 2, kind: "actions", q: "b" }, { seq: 4 }),
  ];

  it("folds only the active turn's entries", () => {
    const other = withEvent({ t: "run.done", responses: 9 }, { seq: 5, invocationId: "inv-older" });
    const s = foldProgress([...entries, other], INV);
    // The older turn's completion must not settle this one.
    expect(s.stage).toBe("fanout");
    expect(s.inquiries).toHaveLength(2);
  });

  it("is empty when no turn is active", () => {
    expect(foldProgress(entries, null)).toEqual(emptyProgress());
  });

  it("ignores console lines that are not events", () => {
    const noise = entry({ seq: 9, message: "[multi_inquire:inquiry:0] some streamed token" });
    expect(foldProgress([...entries, noise], INV).inquiries).toHaveLength(2);
  });

  it("survives seeing a run only from its middle", () => {
    // A busy shared console can evict a run's early entries before the widget
    // first tails it.
    const s = foldProgress([withEvent({ t: "inquiry.finished", i: 1, status: "ok", ok: true }, { seq: 40 })], INV);
    expect(s.inquiries).toEqual([{ index: 1, phase: "done", status: "ok" }]);
    expect(s.expected).toBe(2);
  });
});

describe("labels", () => {
  it("names each phase of a run", () => {
    const at = (events: ProgressEvent[]) => stageLabel(fold(events));
    expect(at([{ t: "run.started", n: 3 }])).toBe("recalling");
    expect(at([{ t: "recall.done" }, { t: "context.done" }])).toBe("reading context");
    expect(at([{ t: "intent.started" }])).toBe("thinking");
    expect(at([{ t: "intent.done", mode: "inquire", n: 2 }])).toBe("planning");
    expect(
      at([{ t: "intent.done", mode: "inquire", n: 2 }, { t: "inquiry.planned", i: 0, q: "a" }]),
    ).toBe("1 inquiry searching");
    expect(at([{ t: "intent.done", mode: "direct", n: 0 }])).toBe("assembling");
    expect(at([{ t: "run.failed", reason: "all_inquiries_failed" }])).toBe("every inquiry failed");
  });

  it("labels each inquiry chip by its phase", () => {
    expect(inquiryChipLabel({ index: 0, phase: "planned" })).toBe("#1 queued");
    expect(inquiryChipLabel({ index: 0, phase: "searched", hits: 7 })).toBe("#1 7 hits");
    expect(inquiryChipLabel({ index: 1, phase: "running" })).toBe("#2 running");
    expect(inquiryChipLabel({ index: 2, phase: "done" })).toBe("#3 done");
    expect(inquiryChipLabel({ index: 2, phase: "failed" })).toBe("#3 failed");
  });

  it("puts scope, question, hits and phase in the hover text", () => {
    expect(
      inquiryTooltip({ index: 1, kind: "documents", question: "what is auth?", hits: 7, phase: "running" }),
    ).toBe('#2 documents\n"what is auth?"\n7 hits\nrunning');
  });

  it("degrades the hover text to what it actually knows", () => {
    expect(inquiryTooltip({ index: 0, phase: "planned" })).toBe("#1\nplanned");
  });

  it("names a non-ok terminal status rather than hiding it behind done", () => {
    expect(inquiryTooltip({ index: 0, phase: "done", status: "timeout" })).toBe("#1\ntimeout");
    expect(inquiryTooltip({ index: 0, phase: "failed", reason: "llm_error" })).toBe(
      "#1\nfailed (llm_error)",
    );
  });

  it("does not claim an inquiry is still running once the turn has ended", () => {
    // The feed is gated on `busy`, so the batch carrying an inquiry's
    // completion can be discarded when the turn finishes. Saying "running"
    // beside a "done" headline is a contradiction; saying "done" would be an
    // invention. Neither.
    const running = { index: 1, phase: "running" as const, question: "q" };
    expect(inquiryChipLabel(running, true)).toBe("#2 ended");
    expect(inquiryChipClass("running", true)).toBe("");
    expect(phaseWord(running, true)).toBe("running (no completion reported)");
    expect(inquiryTooltip(running, true)).toContain("no completion reported");
  });

  it("leaves a genuinely terminal inquiry alone when the turn has ended", () => {
    expect(inquiryChipLabel({ index: 0, phase: "done" }, true)).toBe("#1 done");
    expect(inquiryChipLabel({ index: 0, phase: "failed" }, true)).toBe("#1 failed");
    expect(inquiryChipClass("done", true)).toBe("ok");
    expect(inquiryChipClass("failed", true)).toBe("danger");
  });

  it("says hit rather than hits for one", () => {
    expect(inquiryTooltip({ index: 0, hits: 1, phase: "searched" })).toBe("#1\n1 hit\nsearched");
  });
});

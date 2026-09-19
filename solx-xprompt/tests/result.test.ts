/**
 * Normalising results and transcript entries.
 *
 * The property under test throughout is that nothing reaching a component can
 * make it throw: `TurnBlock` reads `result.intent.mode`, `result.hits`,
 * `result.responses[i].citations` and `result.scripts` without guards, React
 * unmounts the whole tree when a render throws, and the transcript persists —
 * so one bad turn used to blank the widget across reloads.
 */
import { describe, expect, it } from "vitest";
import { normalizeResult, normalizeTranscript, normalizeTurn } from "../src/result";

describe("normalizeResult", () => {
  it("fills every field a component dereferences", () => {
    const r = normalizeResult({});
    expect(r).not.toBeNull();
    expect(r!.intent.mode).toBe("direct");
    expect(r!.intent.inquiries).toEqual([]);
    expect(r!.responses).toEqual([]);
    expect(r!.scripts).toEqual([]);
    expect(r!.hits).toEqual([]);
    expect(r!.notes).toEqual([]);
    expect(r!.errors).toEqual([]);
  });

  it("is null only for something that is not a result at all", () => {
    for (const junk of [null, undefined, 0, "x", [], true]) {
      expect(normalizeResult(junk)).toBeNull();
    }
  });

  it("keeps what is there", () => {
    const r = normalizeResult({
      instruction: "how does auth work?",
      model: "qwen3:4b",
      intent: { mode: "inquire", next_prompt: "check expiry" },
      responses: [{ text: "tokens", citations: ["/notes/auth"], memory: true, tags: ["a"] }],
      notes: ["a note"],
    })!;
    expect(r.instruction).toBe("how does auth work?");
    expect(r.intent.mode).toBe("inquire");
    expect(r.intent.next_prompt).toBe("check expiry");
    expect(r.responses[0]).toMatchObject({ text: "tokens", citations: ["/notes/auth"], memory: true });
    expect(r.notes).toEqual(["a note"]);
  });

  it("gives a response missing its citations an empty list", () => {
    // The exact shape that crashed the render: `r.citations.length` on
    // undefined, from a turn stored before citations existed.
    const r = normalizeResult({ responses: [{ text: "no citations field" }] })!;
    expect(r.responses[0].citations).toEqual([]);
  });

  it("defaults a missing or unknown mode to direct", () => {
    // `inquire` would promise research the turn may never have done.
    expect(normalizeResult({ intent: {} })!.intent.mode).toBe("direct");
    expect(normalizeResult({ intent: { mode: "sideways" } })!.intent.mode).toBe("direct");
    expect(normalizeResult({ intent: null })!.intent.mode).toBe("direct");
  });

  it("gives a script an empty destructive list rather than undefined", () => {
    // Load-bearing: `runScript` gates on it before running anything.
    const r = normalizeResult({ scripts: [{ title: "t", steps: [{ action_ref: "/a/b" }] }] })!;
    expect(r.scripts[0].destructive).toEqual([]);
    expect(r.scripts[0].steps[0]).toEqual({ action_ref: "/a/b", params: {}, capture: null });
  });

  it("drops non-object rows instead of carrying them into a map", () => {
    const r = normalizeResult({ responses: ["oops", null, { text: "real" }], hits: [1, 2] })!;
    expect(r.responses).toHaveLength(1);
    expect(r.responses[0].text).toBe("real");
    expect(r.hits).toEqual([]);
  });

  it("keeps actionType on a hit, since the destructive check reads it", () => {
    const r = normalizeResult({
      hits: [{ source: "action", path: "/x", name: "run", details: { actionType: "command" } }],
    })!;
    expect(r.hits[0].details?.actionType).toBe("command");
  });

  it("treats an unknown hit source as a document", () => {
    const r = normalizeResult({ hits: [{ path: "/x", name: "y" }] })!;
    expect(r.hits[0].source).toBe("document");
  });

  it("survives arrays where objects belong and objects where arrays belong", () => {
    expect(() =>
      normalizeResult({ intent: [], responses: {}, scripts: "no", hits: 4, notes: {} }),
    ).not.toThrow();
    const r = normalizeResult({ intent: [], responses: {}, scripts: "no", hits: 4, notes: {} })!;
    expect(r.responses).toEqual([]);
    expect(r.scripts).toEqual([]);
    expect(r.hits).toEqual([]);
    expect(r.notes).toEqual([]);
  });
});

describe("normalizeTurn", () => {
  it("passes a well-formed turn of each kind through", () => {
    expect(normalizeTurn({ kind: "user", text: "hi", at: "t" })).toEqual({
      kind: "user",
      text: "hi",
      at: "t",
    });
    expect(normalizeTurn({ kind: "error", message: "boom", at: "t" })).toEqual({
      kind: "error",
      message: "boom",
      at: "t",
    });
    expect(normalizeTurn({ kind: "run", status: "skipped", message: "m", at: "t" })).toEqual({
      kind: "run",
      status: "skipped",
      message: "m",
      at: "t",
    });
  });

  it("drops an answer turn whose result is unusable", () => {
    expect(normalizeTurn({ kind: "answer", model: "m", result: null, at: "t" })).toBeNull();
  });

  it("repairs an answer turn whose result is merely incomplete", () => {
    const turn = normalizeTurn({ kind: "answer", model: "m", result: { instruction: "x" }, at: "t" });
    expect(turn).not.toBeNull();
    expect(turn).toMatchObject({ kind: "answer", model: "m" });
  });

  it("drops an unknown kind and a non-object", () => {
    expect(normalizeTurn({ kind: "something-else" })).toBeNull();
    expect(normalizeTurn("nope")).toBeNull();
    expect(normalizeTurn(null)).toBeNull();
  });

  it("coerces an unrecognised run status rather than dropping the turn", () => {
    expect(normalizeTurn({ kind: "run", status: "weird", message: "m" })).toMatchObject({
      status: "ok",
    });
  });
});

describe("normalizeTranscript", () => {
  it("keeps the good entries and drops the bad ones", () => {
    const out = normalizeTranscript([
      { kind: "user", text: "one", at: "" },
      { kind: "answer", result: null, at: "" },
      "junk",
      { kind: "error", message: "boom", at: "" },
    ]);
    expect(out.map((t) => t.kind)).toEqual(["user", "error"]);
  });

  it("is empty for anything that is not an array", () => {
    expect(normalizeTranscript(null)).toEqual([]);
    expect(normalizeTranscript({ kind: "user" })).toEqual([]);
  });
});

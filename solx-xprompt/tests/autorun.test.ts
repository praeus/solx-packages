/**
 * Auto-run tests: capture substitution, destructive gating, and the
 * step-by-step behaviour of `runScript`. Exercised without a React tree —
 * the dispatcher is pure plumbing over `host.try`.
 *
 * `isDestructiveHit` is still tested here even though only plans are
 * executable: the "Run plan" button reads it to decide what to warn about,
 * and its answers must agree with solx-inquiry's `search::is_destructive`,
 * which is what builds a script's `destructive[]` in the first place.
 */
import { describe, expect, it } from "vitest";
import { type Attempt, type Host } from "../../solx-widgets/src/wrap/host";
import {
  buildFollowUpInstruction,
  captureReference,
  isDestructiveHit,
  runScript,
  substituteCaptures,
  unapprovedDestructiveRefs,
  type StepOutcome,
} from "../src/dispatch";
import { clampAutoTurns } from "../src/XPromptWidget";
import type { InquireHit, MultiInquireResult, MultiInquireScript } from "../src/types";

/**
 * A `Host` whose `try()` and `call()` resolve against a scripted response
 * table. Avoids the invocations-poll machinery in `fakeClient` because
 * auto-run fires one direct `client.actions.exec` per step.
 */
function scriptHost(responses: Map<string, { ok: true; value: unknown } | { ok: false; error: string }>): Host {
  return {
    async try<T>(ref: string): Promise<Attempt<T>> {
      const r = responses.get(ref);
      if (!r) return { ok: false, error: "no response scripted for " + ref };
      return r.ok
        ? { ok: true, value: r.value as T }
        : { ok: false, error: r.error };
    },
    async call<T>(ref: string): Promise<T> {
      const r = responses.get(ref);
      if (!r) throw new Error("no response scripted for " + ref);
      if (!r.ok) throw new Error(r.error);
      return r.value as T;
    },
  };
}

// ─── captureReference ──────────────────────────────────────────────────────

describe("captureReference", () => {
  it("accepts $name", () => {
    expect(captureReference("$hits")).toEqual({ name: "hits", path: [] });
  });

  it("accepts $name.field", () => {
    expect(captureReference("$doc.path")).toEqual({ name: "doc", path: ["path"] });
  });

  it("accepts $name.field.sub.thing", () => {
    expect(captureReference("$page.items.0")).toEqual({ name: "page", path: ["items", "0"] });
  });

  it("rejects spliced references", () => {
    expect(captureReference("see $hits for details")).toBeNull();
    expect(captureReference("costs $5.00")).toBeNull();
  });

  it("rejects malformed names", () => {
    expect(captureReference("$")).toBeNull();
    expect(captureReference("$1abc")).toBeNull();
    expect(captureReference("$hits.")).toBeNull();
  });

  it("rejects non-string", () => {
    expect(captureReference("plain")).toBeNull();
  });
});

// ─── substituteCaptures ─────────────────────────────────────────────────────

describe("substituteCaptures", () => {
  it("substitutes a top-level reference", () => {
    const out = substituteCaptures({ q: "$hits" }, { hits: [1, 2, 3] });
    expect(out).toEqual({ q: [1, 2, 3] });
  });

  it("substitutes a dotted reference", () => {
    const out = substituteCaptures(
      { path: "$doc.path" },
      { doc: { path: "/x/y", title: "t" } },
    );
    expect(out).toEqual({ path: "/x/y" });
  });

  it("walks nested objects and arrays", () => {
    const out = substituteCaptures(
      { a: { b: ["$x", "$y"], c: { d: "$x.field" } } },
      { x: { field: "f" }, y: "y" },
    );
    // `$x` → the whole `{field: "f"}` object; `$y` → the string "y";
    // `$x.field` → "f".
    expect(out).toEqual({ a: { b: [{ field: "f" }, "y"], c: { d: "f" } } });
  });

  it("leaves unknown references as-is", () => {
    const out = substituteCaptures({ q: "$unknown" }, {});
    expect(out).toEqual({ q: "$unknown" });
  });

  it("leaves references to missing path segments as-is", () => {
    const out = substituteCaptures({ q: "$doc.missing" }, { doc: { path: "/x" } });
    expect(out).toEqual({ q: "$doc.missing" });
  });

  it("leaves non-reference strings alone", () => {
    const out = substituteCaptures({ q: "see $hits for details" }, { hits: [1] });
    expect(out).toEqual({ q: "see $hits for details" });
  });

  it("does not mutate the input captures", () => {
    const captures = { hits: [1, 2] };
    substituteCaptures({ q: "$hits" }, captures);
    expect(captures).toEqual({ hits: [1, 2] });
  });
});

// ─── isDestructiveHit ───────────────────────────────────────────────────────

const baseHit = (overrides: Partial<InquireHit>): InquireHit => ({
  source: "action",
  path: "/x/y",
  name: "z",
  ...overrides,
});

describe("isDestructiveHit", () => {
  // These mirror solx-inquiry's own `is_destructive_reads_the_tag_and_the_executable_action_types`
  // (search.rs). The two must agree: the widget gates on this, and the
  // `destructive[]` list on a script is built from the Rust side's answer.
  it("flags a command actionType", () => {
    expect(isDestructiveHit(baseHit({ details: { actionType: "command" } }))).toBe(true);
  });

  it("flags a webhook actionType", () => {
    expect(isDestructiveHit(baseHit({ details: { actionType: "webhook" } }))).toBe(true);
  });

  it("flags a command actionType even with no capabilities at all", () => {
    // The hole this replaced: a Command action is shell execution whatever
    // its capabilities say, and reading `category` meant it read as safe.
    expect(
      isDestructiveHit(baseHit({ details: { category: "ops", actionType: "command" } })),
    ).toBe(true);
  });

  it("does not flag a non-executable actionType", () => {
    expect(isDestructiveHit(baseHit({ details: { actionType: "wasm" } }))).toBe(false);
    expect(isDestructiveHit(baseHit({ details: { actionType: "internal" } }))).toBe(false);
  });

  it("flags the solx:destructive capability", () => {
    expect(
      isDestructiveHit(baseHit({ details: { category: "read", capabilities: ["solx:destructive"] } })),
    ).toBe(true);
  });

  it("does not flag a capability that merely resembles the tag", () => {
    // `includes` would have matched both of these; solx-inquiry compares the
    // whole string, and so must this.
    expect(isDestructiveHit(baseHit({ details: { capabilities: ["destructive"] } }))).toBe(false);
    expect(isDestructiveHit(baseHit({ details: { capabilities: ["not-solx:destructive"] } }))).toBe(false);
  });

  it("never reads category as an actionType", () => {
    // `category` is a descriptive grouping ("ops", "llm", "read"). solx-inquiry
    // asserts the same thing from the other side.
    expect(isDestructiveHit(baseHit({ details: { category: "command" } }))).toBe(false);
    expect(isDestructiveHit(baseHit({ details: { category: "webhook" } }))).toBe(false);
    expect(isDestructiveHit(baseHit({ details: { category: "ops" } }))).toBe(false);
  });

  it("returns false for non-action hits", () => {
    expect(
      isDestructiveHit(baseHit({ source: "document", details: { actionType: "command" } })),
    ).toBe(false);
  });

  it("returns false for safe action hits", () => {
    expect(
      isDestructiveHit(baseHit({ details: { category: "read", capabilities: ["read"], actionType: "wasm" } })),
    ).toBe(false);
  });

  it("returns false when there are no details at all", () => {
    expect(isDestructiveHit(baseHit({}))).toBe(false);
    expect(isDestructiveHit(baseHit({ details: null }))).toBe(false);
  });
});

// ─── unapprovedDestructiveRefs ──────────────────────────────────────────────

describe("unapprovedDestructiveRefs", () => {
  const script: MultiInquireScript = {
    title: "t",
    actions: ["/a/b", "/a/c"],
    destructive: ["/a/b", "/a/c"],
    notes: [],
    steps: [],
  };

  it("returns every destructive ref when approved is empty", () => {
    expect(unapprovedDestructiveRefs(script, new Set())).toEqual(["/a/b", "/a/c"]);
  });

  it("omits the approved refs", () => {
    expect(unapprovedDestructiveRefs(script, new Set(["/a/b"]))).toEqual(["/a/c"]);
  });

  it("returns empty when all approved", () => {
    expect(unapprovedDestructiveRefs(script, new Set(["/a/b", "/a/c"]))).toEqual([]);
  });
});

// ─── runScript ──────────────────────────────────────────────────────────────

describe("runScript", () => {
  it("runs steps in order, substituting captures", async () => {
    const calls: Array<{ ref: string; params: unknown }> = [];
    const host: Host = {
      async try<T>(ref: string, params: unknown): Promise<Attempt<T>> {
        calls.push({ ref, params });
        if (ref === "/a/search") return { ok: true, value: { items: ["x"] } as T };
        if (ref === "/a/fetch") return { ok: true, value: { name: "doc-1" } as T };
        return { ok: true, value: { ok: true } as T };
      },
      async call<T>(ref: string, params: unknown): Promise<T> {
        calls.push({ ref, params });
        return undefined as T;
      },
    };

    const script: MultiInquireScript = {
      title: "fetch then format",
      actions: ["/a/search", "/a/fetch", "/a/format"],
      destructive: [],
      notes: [],
      steps: [
        { action_ref: "/a/search", params: { q: "x" }, capture: "hits" },
        {
          action_ref: "/a/fetch",
          params: { id: "$hits.items.0" },
          capture: "doc",
        },
        {
          action_ref: "/a/format",
          params: { doc: "$doc.name" },
          capture: null,
        },
      ],
    };

    const outcomes = await runScript(host, script, { onStep: () => {} });
    expect(outcomes.map((o) => o.status)).toEqual(["ok", "ok", "ok"]);
    // `$hits.items.0` → "x" (the array index), `$doc.name` → "doc-1".
    expect(calls[1].params).toEqual({ id: "x" });
    expect(calls[2].params).toEqual({ doc: "doc-1" });
  });

  it("stops on first error and skips later steps", async () => {
    const host = scriptHost(
      new Map([
        ["/a/ok", { ok: true, value: 1 }],
        // /a/broken deliberately missing → try() resolves to failure
      ]),
    );
    const script: MultiInquireScript = {
      title: "t",
      actions: ["/a/ok", "/a/broken", "/a/never"],
      destructive: [],
      notes: [],
      steps: [
        { action_ref: "/a/ok", params: {}, capture: null },
        { action_ref: "/a/broken", params: {}, capture: null },
        { action_ref: "/a/never", params: {}, capture: null },
      ],
    };
    const outcomes = await runScript(host, script);
    expect(outcomes.map((o) => o.status)).toEqual(["ok", "error"]);
  });

  it("refuses when there are unapproved destructive refs and refuseOnUnapproved is true", async () => {
    const host = scriptHost(
      new Map([
        ["/a/ok", { ok: true, value: 1 }],
        ["/a/danger", { ok: true, value: "did it" }],
      ]),
    );
    const script: MultiInquireScript = {
      title: "t",
      actions: ["/a/ok", "/a/danger"],
      destructive: ["/a/danger"],
      notes: [],
      steps: [
        { action_ref: "/a/ok", params: {}, capture: null },
        { action_ref: "/a/danger", params: {}, capture: null },
      ],
    };
    const outcomes = await runScript(host, script, { approved: new Set() });
    expect(outcomes.every((o) => o.status === "skipped")).toBe(true);
  });

  it("runs approved destructive steps and skips unapproved ones when refuseOnUnapproved is false", async () => {
    const host = scriptHost(
      new Map([
        ["/a/ok", { ok: true, value: 1 }],
        ["/a/danger", { ok: true, value: "did it" }],
      ]),
    );
    const script: MultiInquireScript = {
      title: "t",
      actions: ["/a/ok", "/a/danger"],
      destructive: ["/a/danger"],
      notes: [],
      steps: [
        { action_ref: "/a/ok", params: {}, capture: null },
        { action_ref: "/a/danger", params: {}, capture: null },
      ],
    };
    // refuseOnUnapproved=false means "don't refuse the whole script" — but
    // an individual unapproved-destructive step still skips itself, just
    // without the script-level bail. Here the approved set is empty, so the
    // destructive step is skipped and the safe step runs.
    const outcomes = await runScript(host, script, {
      approved: new Set(),
      refuseOnUnapproved: false,
    });
    expect(outcomes.map((o) => o.status)).toEqual(["ok", "skipped"]);
  });

  it("runs destructive steps when their ref is in the approved set, even with refuseOnUnapproved false", async () => {
    const host = scriptHost(
      new Map([
        ["/a/ok", { ok: true, value: 1 }],
        ["/a/danger", { ok: true, value: "did it" }],
      ]),
    );
    const script: MultiInquireScript = {
      title: "t",
      actions: ["/a/ok", "/a/danger"],
      destructive: ["/a/danger"],
      notes: [],
      steps: [
        { action_ref: "/a/ok", params: {}, capture: null },
        { action_ref: "/a/danger", params: {}, capture: null },
      ],
    };
    const outcomes = await runScript(host, script, {
      approved: new Set(["/a/danger"]),
      refuseOnUnapproved: false,
    });
    expect(outcomes.map((o) => o.status)).toEqual(["ok", "ok"]);
  });

  it("invokes onStep with one outcome per step", async () => {
    const host = scriptHost(
      new Map([
        ["/a/search", { ok: true, value: 1 }],
        ["/a/fmt", { ok: true, value: 1 }],
      ]),
    );
    const script: MultiInquireScript = {
      title: "t",
      actions: ["/a/search", "/a/fmt"],
      destructive: [],
      notes: [],
      steps: [
        { action_ref: "/a/search", params: {}, capture: null },
        { action_ref: "/a/fmt", params: {}, capture: null },
      ],
    };
    const seen: StepOutcome[] = [];
    await runScript(host, script, { onStep: (o) => seen.push(o) });
    expect(seen.length).toBe(2);
    expect(seen[0].status).toBe("ok");
    expect(seen[1].status).toBe("ok");
  });
});

// ─── cancellation and malformed input ───────────────────────────────────────

describe("runScript cancellation", () => {
  const twoStep: MultiInquireScript = {
    title: "t",
    actions: ["/a/one", "/a/two"],
    destructive: [],
    notes: [],
    steps: [
      { action_ref: "/a/one", params: {}, capture: null },
      { action_ref: "/a/two", params: {}, capture: null },
    ],
  };

  it("stops before the next step once shouldContinue goes false", async () => {
    const ran: string[] = [];
    let alive = true;
    const host: Host = {
      async try<T>(ref: string): Promise<Attempt<T>> {
        ran.push(ref);
        alive = false; // the user hits Stop while step 1 is in flight
        return { ok: true, value: 1 as T };
      },
      async call<T>(): Promise<T> {
        throw new Error("unused");
      },
    };
    const outcomes = await runScript(host, twoStep, { shouldContinue: () => alive });
    // Step 1 already happened - there is no interrupting a call in flight.
    // The guarantee is that step 2 never starts.
    expect(ran).toEqual(["/a/one"]);
    expect(outcomes.map((o) => o.status)).toEqual(["ok", "skipped"]);
    expect(outcomes[1].message).toContain("cancelled");
  });

  it("runs nothing at all when cancelled before the first step", async () => {
    const ran: string[] = [];
    const host: Host = {
      async try<T>(ref: string): Promise<Attempt<T>> {
        ran.push(ref);
        return { ok: true, value: 1 as T };
      },
      async call<T>(): Promise<T> {
        throw new Error("unused");
      },
    };
    const outcomes = await runScript(host, twoStep, { shouldContinue: () => false });
    expect(ran).toEqual([]);
    expect(outcomes).toHaveLength(1);
    expect(outcomes[0].status).toBe("skipped");
  });

  it("runs to completion when shouldContinue is not supplied", async () => {
    const host = scriptHost(
      new Map([
        ["/a/one", { ok: true, value: 1 }],
        ["/a/two", { ok: true, value: 2 }],
      ]),
    );
    const outcomes = await runScript(host, twoStep);
    expect(outcomes.map((o) => o.status)).toEqual(["ok", "ok"]);
  });
});

describe("malformed scripts", () => {
  // Results are structural only (see types.ts), and a script can arrive from
  // a session document written by an older build. An unguarded
  // `script.destructive.some(...)` threw before the destructive gate ran.
  it("does not throw when destructive is missing", async () => {
    const host = scriptHost(new Map([["/a/one", { ok: true, value: 1 }]]));
    const script = {
      title: "t",
      actions: ["/a/one"],
      notes: [],
      steps: [{ action_ref: "/a/one", params: {}, capture: null }],
    } as unknown as MultiInquireScript;
    const outcomes = await runScript(host, script);
    expect(outcomes.map((o) => o.status)).toEqual(["ok"]);
  });

  it("does not throw when steps is missing", async () => {
    const host = scriptHost(new Map());
    const script = { title: "t", actions: [], destructive: [], notes: [] } as unknown as MultiInquireScript;
    await expect(runScript(host, script)).resolves.toEqual([]);
  });
});

// ─── buildFollowUpInstruction ───────────────────────────────────────────────

const emptyResult = (over: Partial<MultiInquireResult> = {}): MultiInquireResult =>
  ({
    instruction: "how does auth work?",
    model: "m",
    session: "/s/n",
    intent: { mode: "inquire", inquiries: [], next_prompt: "check token expiry" },
    responses: [],
    memories: [],
    scripts: [],
    hits: [],
    notes: [],
    errors: [],
    ...over,
  }) as MultiInquireResult;

const response = (text: string, citations: string[] = []) =>
  ({ text, memory: false, tags: [], citations, inquiry: 0 }) as MultiInquireResult["responses"][number];

describe("buildFollowUpInstruction", () => {
  it("carries the previous turn's findings into the instruction", () => {
    const out = buildFollowUpInstruction(
      "check token expiry",
      emptyResult({ responses: [response("Auth uses session tokens.", ["/notes/auth"])] }),
    );
    expect(out.startsWith("check token expiry")).toBe(true);
    expect(out).toContain("Auth uses session tokens.");
    expect(out).toContain("/notes/auth");
    expect(out).toContain("Previously asked: how does auth work?");
    // The framing has to be explicit about *why* the suggestion is suspect.
    expect(out).toContain("before that turn had searched anything");
  });

  it("returns the suggestion unchanged when the turn found nothing", () => {
    // A preamble promising results that are not there is worse than none:
    // the model tends to satisfy it by inventing them.
    expect(buildFollowUpInstruction("do the next thing", emptyResult())).toBe("do the next thing");
  });

  it("does not count the restated question as a finding on its own", () => {
    const out = buildFollowUpInstruction("next", emptyResult({ instruction: "only this" }));
    expect(out).toBe("next");
  });

  it("names proposed plans without inlining their steps", () => {
    const out = buildFollowUpInstruction(
      "next",
      emptyResult({
        scripts: [
          {
            title: "rotate tokens",
            actions: ["/a/b"],
            destructive: ["/a/b"],
            notes: [],
            steps: [{ action_ref: "/a/b", params: {}, capture: null }],
          },
        ],
      }),
    );
    expect(out).toContain('proposed a plan "rotate tokens" (1 step(s), 1 destructive, not run)');
    expect(out).not.toContain("action_ref");
  });

  it("says when the findings are partial", () => {
    const out = buildFollowUpInstruction(
      "next",
      emptyResult({ responses: [response("partial answer")], errors: [{ inquiry: 1 }] }),
    );
    expect(out).toContain("1 inquiry failed, so the findings above are partial");
  });

  it("pluralises failed inquiries", () => {
    const out = buildFollowUpInstruction(
      "next",
      emptyResult({ responses: [response("a")], errors: [{}, {}] }),
    );
    expect(out).toContain("2 inquiries failed");
  });

  it("truncates a long response rather than sending the whole thing", () => {
    const long = "x".repeat(5000);
    const out = buildFollowUpInstruction("next", emptyResult({ responses: [response(long)] }));
    expect(out).not.toContain(long);
    expect(out).toContain("…");
    expect(out.length).toBeLessThan(3000);
  });

  it("keeps the whole block within budget when a turn produced a lot", () => {
    const many = Array.from({ length: 40 }, (_, i) => response(`finding ${i} ` + "y".repeat(400)));
    const out = buildFollowUpInstruction("next", emptyResult({ responses: many }));
    // Suggestion + preamble + a bounded findings block, not 40 responses.
    expect(out.length).toBeLessThan(3000);
    expect(out).toContain("finding 0");
  });

  it("survives a result with fields missing entirely", () => {
    // A turn replayed from a session document written by an older build.
    const partial = { instruction: "x" } as unknown as MultiInquireResult;
    expect(() => buildFollowUpInstruction("next", partial)).not.toThrow();
    expect(buildFollowUpInstruction("next", partial)).toBe("next");
  });

  it("ignores blank responses", () => {
    const out = buildFollowUpInstruction(
      "next",
      emptyResult({ responses: [response("   "), response("")] }),
    );
    expect(out).toBe("next");
  });
});

// ─── clampAutoTurns ─────────────────────────────────────────────────────────

describe("clampAutoTurns", () => {
  // The only bound on a loop that spends 1 + N model calls per turn, so it
  // is enforced where the value is *read*, not only where it is typed.
  it("holds values already in range", () => {
    expect(clampAutoTurns(1)).toBe(1);
    expect(clampAutoTurns(3)).toBe(3);
    expect(clampAutoTurns(10)).toBe(10);
  });

  it("clamps a hand-edited localStorage value", () => {
    expect(clampAutoTurns(999)).toBe(10);
    expect(clampAutoTurns(0)).toBe(1);
    expect(clampAutoTurns(-5)).toBe(1);
  });

  it("falls back for a value that is not a number at all", () => {
    expect(clampAutoTurns(NaN)).toBe(3);
    expect(clampAutoTurns(Infinity)).toBe(3);
  });

  it("truncates rather than rounding up", () => {
    expect(clampAutoTurns(3.9)).toBe(3);
  });
});

/**
 * `plan.ts` against the shapes a model actually returns.
 *
 * The point of these is not that a well-formed plan survives -- that is the
 * easy case -- but that everything a small model gets wrong is absorbed rather
 * than thrown back at it. A wrong verb, a `get` with half a reference, a
 * sentence in `q`, more steps than the cap, a `direct` beside real steps: each
 * has a defined, non-fatal outcome, and none of them errors the turn.
 */

import { describe, expect, test } from "vitest";
import { cleanPlan, planIsEmpty, PLAN_SCHEMA } from "../src/harness/plan";
import { MAX_PLAN_STEPS_HARD } from "../src/harness/refs";

describe("cleanPlan", () => {
  test("keeps a well-formed plan intact", () => {
    const { steps, notes } = cleanPlan(
      {
        steps: [
          { verb: "search", q: "rust async", path_prefix: "/blog", limit: 5 },
          { verb: "list", path_prefix: "/research", limit: 20 },
        ],
      },
      4,
    );
    expect(notes).toEqual([]);
    expect(steps).toHaveLength(2);
    expect(steps[0]).toMatchObject({ verb: "search", q: "rust async", path_prefix: "/blog", limit: 5 });
    expect(steps[1]).toMatchObject({ verb: "list", path_prefix: "/research", limit: 20 });
  });

  test("drops an unknown verb with a note rather than failing", () => {
    const { steps, notes } = cleanPlan(
      { steps: [{ verb: "delete_everything", q: "x" }, { verb: "search", q: "ok" }] },
      4,
    );
    expect(steps).toHaveLength(1);
    expect(steps[0].verb).toBe("search");
    expect(notes.some((n) => n.includes("verb"))).toBe(true);
  });

  test("drops a get that has only one half of its reference", () => {
    const { steps, notes } = cleanPlan({ steps: [{ verb: "get", path: "/blog" }] }, 4);
    expect(steps).toHaveLength(0);
    expect(notes.some((n) => n.includes("get"))).toBe(true);
  });

  test("keeps a get that has both halves", () => {
    const { steps } = cleanPlan({ steps: [{ verb: "get", path: "/blog", name: "recap" }] }, 4);
    expect(steps).toHaveLength(1);
    expect(steps[0]).toMatchObject({ verb: "get", path: "/blog", name: "recap" });
  });

  test("clamps a limit above the hard cap and defaults a missing one", () => {
    const { steps } = cleanPlan(
      { steps: [{ verb: "search", q: "a" , limit: 9999 }, { verb: "search", q: "b" }] },
      4,
    );
    expect(steps[0].limit).toBe(25);
    expect(steps[1].limit).toBe(10);
  });

  test("caps the number of steps and says so", () => {
    const many = { steps: Array.from({ length: 6 }, (_, i) => ({ verb: "search", q: "q" + i })) };
    const { steps, notes } = cleanPlan(many, 3);
    expect(steps).toHaveLength(3);
    expect(notes.some((n) => n.includes("cap"))).toBe(true);
  });

  test("never exceeds the hard cap even when asked to", () => {
    const many = { steps: Array.from({ length: MAX_PLAN_STEPS_HARD + 5 }, (_, i) => ({ verb: "search", q: "q" + i })) };
    const { steps } = cleanPlan(many, 999);
    expect(steps.length).toBeLessThanOrEqual(MAX_PLAN_STEPS_HARD);
  });

  test("a sentence in q survives -- the planner brief warns, but the harness must not refuse", () => {
    const { steps } = cleanPlan(
      { steps: [{ verb: "search", q: "how do I set up the server" }] },
      4,
    );
    expect(steps).toHaveLength(1);
    expect(steps[0].q).toBe("how do I set up the server");
  });

  test("keeps a direct answer only when there are no steps", () => {
    const withSteps = cleanPlan(
      { steps: [{ verb: "search", q: "x" }], direct: "ignored because there are steps" },
      4,
    );
    expect(withSteps.plan.direct).toBeNull();
    expect(withSteps.plan.steps).toHaveLength(1);

    const stepless = cleanPlan({ direct: "Hello. Ask me about your documents." }, 4);
    expect(stepless.plan.direct).toContain("Hello");
    expect(stepless.plan.steps).toHaveLength(0);
  });

  test("survives a planner that returned nothing", () => {
    const { plan, notes } = cleanPlan(undefined, 4);
    expect(plan.steps).toEqual([]);
    expect(notes).toContain("planner returned nothing usable");
  });

  test("survives a planner that returned a non-object", () => {
    const { steps } = cleanPlan("the model wrote prose instead", 4);
    expect(steps).toEqual([]);
  });
});

describe("planIsEmpty", () => {
  test("is true for no steps and no direct", () => {
    expect(planIsEmpty({ steps: [] })).toBe(true);
  });
  test("is false when there is a direct answer", () => {
    expect(planIsEmpty({ steps: [], direct: "hi" })).toBe(false);
  });
  test("is false when there are steps", () => {
    expect(planIsEmpty({ steps: [{ verb: "search" }] })).toBe(false);
  });
});

describe("PLAN_SCHEMA", () => {
  test("names only the five verbs in the enum", () => {
    const enumValues = (PLAN_SCHEMA.properties as Record<string, { items?: { properties?: Record<string, { enum?: string[] }> } }>).steps.items!.properties!.verb.enum;
    expect(enumValues).toEqual(["search", "list", "get", "types", "actions"]);
  });

  test("carries no tool names or action references -- it is not a dispatch table", () => {
    const text = JSON.stringify(PLAN_SCHEMA);
    expect(text).not.toContain("act__");
    expect(text).not.toContain("/builtin/");
  });
});

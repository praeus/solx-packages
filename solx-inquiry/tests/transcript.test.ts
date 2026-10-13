/**
 * The transcript is a shape adapter, not a reconstruction -- so these tests
 * are about the rendering decisions, not invariants. The one that matters is
 * `detailOf`: it is what makes a plan legible, and a plan row that says
 * "search" with no terms tells a reader nothing about what the model meant.
 */

import { describe, expect, test } from "vitest";
import { buildTurns, detailOf, liveLabel, summariseTurn } from "../src/transcript";
import type { InquiryTurn } from "../src/harness";

function turn(overrides: Partial<InquiryTurn> = {}): InquiryTurn {
  return {
    question: "q",
    status: "idle",
    plan: [],
    plan_error: null,
    corpus: "",
    steps: [],
    documents: [],
    answer: null,
    ...overrides,
  };
}

describe("detailOf", () => {
  test("renders a search's terms and path", () => {
    expect(detailOf({ verb: "search", q: "rust async", path_prefix: "/blog" })).toContain("rust async");
    expect(detailOf({ verb: "search", q: "rust async", path_prefix: "/blog" })).toContain("/blog");
  });

  test("renders a get's exact reference", () => {
    expect(detailOf({ verb: "get", path: "/blog", name: "recap" })).toContain("/blog/recap");
  });

  test("renders nothing for a bare list with no filters", () => {
    expect(detailOf({ verb: "list" })).toBe("");
  });
});

describe("buildTurns", () => {
  test("marks a turn live only while a phase is in flight", () => {
    const [live] = buildTurns([turn({ status: "gathering" })]);
    expect(live.liveStatus).toBe("gathering");
    const [settled] = buildTurns([turn({ status: "idle" })]);
    expect(settled.liveStatus).toBeNull();
  });

  test("carries plan, steps, documents and answer through", () => {
    const [t] = buildTurns([
      turn({
        plan: [{ verb: "search", q: "rust" }],
        steps: [
          {
            verb: "search",
            action: "/builtin/document/search-documents",
            params: { q: "rust" },
            outcome: "ok",
            count: 2,
            error: null,
            note: null,
          },
        ],
        documents: [
          { ref: "/blog/a", path: "/blog", name: "a", title: "A", summary: "s", type_ref: null, contents: null },
        ],
        answer: "found it",
      }),
    ]);
    expect(t.plan).toHaveLength(1);
    expect(t.steps[0].outcome).toBe("ok");
    expect(t.documents).toHaveLength(1);
    expect(t.answer).toBe("found it");
  });
});

describe("summariseTurn", () => {
  test("counts lookups and documents", () => {
    const [t] = buildTurns([
      turn({
        steps: [
          { verb: "search", action: "a", params: {}, outcome: "ok", count: 2, error: null, note: null },
          { verb: "get", action: "b", params: {}, outcome: "ok", count: 1, error: null, note: null },
        ],
        documents: [
          { ref: "/x", path: "/x", name: "x", title: "", summary: "", type_ref: null, contents: null },
        ],
      }),
    ]);
    expect(summariseTurn(t)).toContain("2 lookups");
  });

  test("says 'answered directly' when nothing ran", () => {
    const [t] = buildTurns([turn({ answer: "hi" })]);
    expect(summariseTurn(t)).toBe("answered directly");
  });
});

describe("liveLabel", () => {
  test("names each phase", () => {
    expect(liveLabel("planning")).toContain("planning");
    expect(liveLabel("gathering")).toContain("gathering");
    expect(liveLabel("answering")).toContain("answer");
  });
});

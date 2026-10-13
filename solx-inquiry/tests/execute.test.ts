/**
 * `execute.ts` against the fake host.
 *
 * Two things matter here and both are the `q: null` trap in a different
 * costume: `list` must send **no** `q` key at all (an explicit null fails
 * validation), and `search` must send `pathPrefix`/`typeRef` in the camelCase
 * the builtins read. The fake rejects a null key outright, so a regression
 * here fails loudly rather than silently returning nothing.
 */

import { describe, expect, test } from "vitest";
import { executePlan, foundNothing } from "../src/harness/execute";
import { fake, withInquiryActions } from "./fakeHost";

describe("executePlan", () => {
  test("maps each verb to its builtin and sends camelCase params", async () => {
    const base = fake();
    withInquiryActions(base.fake)
      .doc("/blog/rust-1", {
        title: "Async Rust",
        summary: "a post",
        typeRef: "/types/BlogPost",
        contents: { body: "async" },
      });
    base.fake.type("/types/BlogPost", { type: "object" }, "a blog post");

    const result = await executePlan(base.host, [
      { verb: "search", q: "rust", path_prefix: "/blog", type_ref: "/types/BlogPost", limit: 5 },
    ]);

    expect(result.steps[0].action).toBe("/builtin/document/search-documents");
    expect(result.documents).toHaveLength(1);
    expect(result.documents[0].ref).toBe("/blog/rust-1");

    const sent = base.fake.refsCalled("/builtin/document/search-documents")[0].params;
    expect(sent.q).toBe("rust");
    expect(sent.pathPrefix).toBe("/blog");
    expect(sent.typeRef).toBe("/types/BlogPost");
  });

  test("list sends no q key at all, because an explicit null fails validation", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/a", { title: "A", contents: {} });
    const result = await executePlan(base.host, [{ verb: "list", path_prefix: "/blog" }]);

    expect(result.steps[0].outcome).toBe("ok");
    const sent = base.fake.refsCalled("/builtin/document/search-documents")[0].params;
    expect("q" in sent).toBe(false);
    expect(sent.pathPrefix).toBe("/blog");
  });

  test("a term that matches nothing is an empty outcome, not an error", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/a", { title: "A", contents: {} });
    const result = await executePlan(base.host, [{ verb: "search", q: "nonexistentterm" }]);
    expect(result.steps[0].outcome).toBe("empty");
    expect(foundNothing(result)).toBe(true);
  });

  test("get resolves one document by its two halves", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/recap", { title: "Recap", contents: { x: 1 } });
    const result = await executePlan(base.host, [{ verb: "get", path: "/blog", name: "recap" }]);
    expect(result.steps[0].outcome).toBe("ok");
    expect(result.documents[0].ref).toBe("/blog/recap");
  });

  test("de-duplicates a document found by two steps", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/a", { title: "A", summary: "shared term", contents: {} });
    const result = await executePlan(base.host, [
      { verb: "search", q: "shared" },
      { verb: "search", q: "term" },
    ]);
    expect(result.documents).toHaveLength(1);
    // The second step ran and contributed nothing new.
    expect(result.steps[1].count).toBe(0);
  });

  test("stops gathering once the document cap is reached", async () => {
    const base = fake();
    withInquiryActions(base.fake);
    for (let i = 0; i < 10; i++) {
      base.fake.doc("/blog/p" + i, { title: "Post " + i, summary: "common", contents: {} });
    }
    const result = await executePlan(base.host, [{ verb: "search", q: "common" }], 3);
    expect(result.documents).toHaveLength(3);
  });

  test("records a failed step and continues with the ones that work", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/a", { title: "A", summary: "good", contents: {} });
    // A step whose verb maps to no action, plus a real one.
    const result = await executePlan(base.host, [
      { verb: "get", path: "/gone", name: "missing" },
      { verb: "search", q: "good" },
    ]);
    expect(result.steps[0].outcome).toBe("error");
    expect(result.steps[0].error).toContain("not found");
    expect(result.steps[1].outcome).toBe("ok");
    expect(result.documents).toHaveLength(1);
  });

  test("types filters the list client-side by the step's terms", async () => {
    const base = fake();
    base.fake
      .action("/builtin/type/entity-list-types", { description: "list types" })
      .type("/types/BlogPost", { type: "object" }, "a blog post")
      .type("/types/MediaDocument", { type: "object" }, "extracted media");
    const result = await executePlan(base.host, [{ verb: "types", q: "blog" }]);
    expect(result.documents.map((d) => d.name)).toContain("BlogPost");
    expect(result.documents.map((d) => d.name)).not.toContain("MediaDocument");
  });

  test("actions searches the action registry and carries the description as the body", async () => {
    const base = fake();
    withInquiryActions(base.fake);
    const result = await executePlan(base.host, [{ verb: "actions", q: "search" }]);
    expect(result.steps[0].action).toBe("/builtin/action/search-actions");
    expect(result.documents.length).toBeGreaterThan(0);
  });

  test("carries a plan note onto the executed step, so a repair stays visible", async () => {
    const base = fake();
    withInquiryActions(base.fake);
    const result = await executePlan(base.host, [
      { verb: "search", q: "x", note: "dropped a step whose verb was not one of ..." },
    ]);
    expect(result.steps[0].note).toContain("dropped");
  });
});

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

  test("a sentence-shaped query recovers by retrying term by term", async () => {
    // The failure running the package exposed: `q` is ANDed, so "documents
    // skill how a document is identified identifier" matches nothing even
    // though `/agent/skills/documents` exists. The retry must find it.
    const base = fake();
    withInquiryActions(base.fake).doc("/agent/skills/documents", {
      title: "Working with documents",
      summary: "how a document is identified",
      contents: { body: "A document is identified by its path and its name together." },
    });

    const sentence = "documents skill how a document is identified identifier";
    const result = await executePlan(base.host, [{ verb: "search", q: sentence, limit: 10 }]);

    expect(result.steps[0].outcome).toBe("ok");
    expect(result.documents.map((d) => d.ref)).toContain("/agent/skills/documents");
  });

  test("no fallback when the phrase already matched", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/a", {
      title: "Async Rust",
      summary: "rust",
      contents: { body: "rust" },
    });
    await executePlan(base.host, [{ verb: "search", q: "rust", limit: 10 }]);
    // One call: the phrase matched, so no per-term widening happened.
    expect(base.fake.refsCalled("/builtin/document/search-documents")).toHaveLength(1);
  });

  test("a one-word query that matches nothing is not widened", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/a", { title: "A", contents: {} });
    const result = await executePlan(base.host, [{ verb: "search", q: "zzznomatch" }]);
    expect(result.steps[0].outcome).toBe("empty");
    // Still one call: a single term has nothing to widen to.
    expect(base.fake.refsCalled("/builtin/document/search-documents")).toHaveLength(1);
  });

  test("the widened retry de-duplicates a row matched by two terms", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/rust-async", {
      title: "Rust async",
      summary: "rust async guide",
      contents: { body: "rust async guide" },
    });
    const result = await executePlan(base.host, [
      { verb: "search", q: "rust nonexistent async", limit: 10 },
    ]);
    expect(result.documents.filter((d) => d.ref === "/blog/rust-async")).toHaveLength(1);
  });

  test("widens past the package's own transcript, which the phrase matches", async () => {
    // The subtle half of the own-transcript bug, found by running the package
    // against a real server: a saved inquiry stores its question, so the
    // phrase "overwrite existing name save document" genuinely matches -- but
    // only that inquiry. The raw count is non-zero, so a naive "did the page
    // return anything?" check never widens, and the own-transcript filter then
    // removes every match. The retry must judge on *usable* rows.
    const base = fake();
    withInquiryActions(base.fake)
      // The real document, which only a single term ("save") reaches.
      .doc("/agent/skills/documents", {
        title: "Working with documents",
        summary: "saving and searching",
        contents: { body: "entity-save-document is an upsert. Save first." },
      })
      // A prior inquiry whose stored question contains the whole phrase.
      .doc("/inquiry/sessions/old-thread", {
        title: "overwrite existing name save document",
        summary: "",
        contents: { question: "overwrite existing name save document" },
      });

    const result = await executePlan(base.host, [
      { verb: "search", q: "overwrite existing name save document", limit: 10 },
    ]);

    expect(result.documents.map((d) => d.ref)).toContain("/agent/skills/documents");
    expect(result.documents.map((d) => d.ref)).not.toContain("/inquiry/sessions/old-thread");
  });

  test("still returns the transcript when the step asks for the inquiry root", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/inquiry/sessions/prior", {
      title: "a prior thread",
      summary: "summary",
      contents: {},
    });
    const result = await executePlan(base.host, [
      { verb: "search", q: "prior", path_prefix: "/inquiry", limit: 10 },
    ]);
    expect(result.documents.map((d) => d.ref)).toContain("/inquiry/sessions/prior");
  });

  test("list never triggers the term fallback, because it sends no q", async () => {
    const base = fake();
    withInquiryActions(base.fake).doc("/blog/a", { title: "A", contents: {} });
    const result = await executePlan(base.host, [{ verb: "list", path_prefix: "/blog" }]);
    expect(result.steps[0].outcome).toBe("ok");
    expect(base.fake.refsCalled("/builtin/document/search-documents")).toHaveLength(1);
  });
});

describe("scope repair and widening", () => {
  const vocab = { paths: ["/agent/skills", "/inquiry/sessions"], types: ["/packages/solx-agent/AgentSkill"] };

  function store() {
    const base = fake();
    withInquiryActions(base.fake).doc("/agent/skills/javascript-actions", {
      title: "Writing JavaScript actions",
      typeRef: "/packages/solx-agent/AgentSkill",
      contents: { body: "javascript action how-to" },
    });
    return base;
  }

  test("a document path given as type_ref is used as path_prefix", async () => {
    // Observed live: {"verb":"search","q":"javascript action","type_ref":"/agent/skills"}.
    const base = store();
    const result = await executePlan(
      base.host,
      [{ verb: "search", q: "javascript", type_ref: "/agent/skills" }],
      12,
      vocab,
    );
    expect(result.documents.map((d) => d.ref)).toEqual(["/agent/skills/javascript-actions"]);
    expect(result.steps[0].params).toMatchObject({ pathPrefix: "/agent/skills" });
    expect(result.steps[0].params.typeRef).toBeUndefined();
    expect(result.steps[0].note).toContain("path_prefix");
  });

  test("a bare type name resolves to the one known ref with that name", async () => {
    const base = store();
    const result = await executePlan(base.host, [{ verb: "search", q: "javascript", type_ref: "AgentSkill" }], 12, vocab);
    expect(result.steps[0].params.typeRef).toBe("/packages/solx-agent/AgentSkill");
    expect(result.documents).toHaveLength(1);
  });

  test("an unknown type that matches nothing is shed, and the step says so", async () => {
    // Observed live: type_ref "SearchDocumentsParams", a registry type no document has.
    const base = store();
    const result = await executePlan(
      base.host,
      [{ verb: "search", q: "javascript", type_ref: "SearchDocumentsParams" }],
      12,
      vocab,
    );
    expect(result.documents).toHaveLength(1);
    expect(result.steps[0].params.typeRef).toBeUndefined();
    expect(result.steps[0].note).toContain("searched without it");
  });

  test("an invented path prefix is shed; a known one that finds nothing is not", async () => {
    const base = store();
    const invented = await executePlan(base.host, [{ verb: "search", q: "javascript", path_prefix: "/docs" }], 12, vocab);
    expect(invented.documents).toHaveLength(1);

    const known = await executePlan(
      base.host,
      [{ verb: "search", q: "zzznomatch", path_prefix: "/agent/skills" }],
      12,
      vocab,
    );
    expect(known.documents).toHaveLength(0);
    expect(known.steps[0].params.pathPrefix).toBe("/agent/skills");
  });

  test("a term-less list never sheds its path, which would enumerate the store", async () => {
    const base = store();
    const result = await executePlan(base.host, [{ verb: "list", path_prefix: "/docs" }], 12, vocab);
    expect(result.documents).toHaveLength(0);
  });
});

describe("the inquiry's own document", () => {
  test("is never gathered, even when the step scopes to the inquiry root", async () => {
    const base = fake();
    withInquiryActions(base.fake)
      .doc("/inquiry/sessions/older", { title: "Berlin sales 2024", contents: {} })
      .doc("/inquiry/sessions/this-one", { title: "Berlin sales 2024", contents: {} });
    const result = await executePlan(
      base.host,
      [{ verb: "search", q: "Berlin", path_prefix: "/inquiry/sessions" }],
      12,
      undefined,
      ["/inquiry/sessions/this-one"],
    );
    expect(result.documents.map((d) => d.ref)).toEqual(["/inquiry/sessions/older"]);
  });
});

/**
 * The corpus index against the fake host.
 *
 * This is the piece that makes a small model usable -- it copies a real path
 * instead of inventing one -- so the assertions are about what the model is
 * *shown*: the paths, most-populated first, and the types, with the action
 * paths only when asked for.
 */

import { describe, expect, test } from "vitest";
import { buildCorpus, renderCorpus, resolveCorpusPaths } from "../src/harness/corpus";
import { fake, withInquiryActions } from "./fakeHost";

describe("resolveCorpusPaths", () => {
  test("returns distinct paths with counts, most-populated first", async () => {
    const base = fake();
    base.fake
      .action("/builtin/document/entity-list-document-paths", { description: "paths" })
      .doc("/blog/a", {})
      .doc("/blog/b", {})
      .doc("/research/c", {});
    const paths = await resolveCorpusPaths(base.host);
    expect(paths[0]).toMatchObject({ path: "/blog", count: 2 });
    expect(paths.map((p) => p.path)).toContain("/research");
  });

  test("fails soft when the action is absent", async () => {
    const base = fake();
    const paths = await resolveCorpusPaths(base.host);
    expect(paths).toEqual([]);
  });
});

describe("buildCorpus", () => {
  test("includes the types documents use, as full refs, but not action paths by default", async () => {
    const base = fake();
    withInquiryActions(base.fake)
      .action("/builtin/document/entity-list-document-paths", { description: "paths" })
      .doc("/blog/a", { title: "A", typeRef: "/types/BlogPost" });
    // Registered but used by no document: the planner must not be shown it,
    // because a type filter no document matches finds nothing.
    base.fake.type("/builtin/types/SearchDocumentsParams", { type: "object" }, "params");

    const corpus = await buildCorpus(base.host, false);
    expect(corpus.text).toContain("/blog");
    expect(corpus.types).toEqual(["/types/BlogPost"]);
    expect(corpus.text).not.toContain("SearchDocumentsParams");
    expect(corpus.text).not.toContain("Action paths:");
  });

  test("includes action paths when asked", async () => {
    const base = fake();
    withInquiryActions(base.fake);
    const corpus = await buildCorpus(base.host, true);
    expect(corpus.text).toContain("Action paths:");
  });

  test("sends no excludeHidden to a plain ListOptions endpoint", async () => {
    // `excludeHidden` exists only on `ActionSearchQuery`. On a `ListOptions`
    // schema it is not a rejection -- it is *silently dropped*, which is worse:
    // the caller believes a filter happened. `entity-list-document-paths` and
    // `entity-list-types` take plain `ListOptions`, so neither may carry it.
    const base = fake();
    withInquiryActions(base.fake)
      .action("/builtin/document/entity-list-document-paths", { description: "paths" })
      .doc("/blog/a", {});
    base.fake.type("/types/BlogPost", { type: "object" }, "a post");

    await buildCorpus(base.host, false);

    for (const ref of ["/builtin/document/entity-list-document-paths", "/builtin/type/entity-list-types"]) {
      for (const call of base.fake.refsCalled(ref)) {
        expect("excludeHidden" in call.params).toBe(false);
      }
    }
  });

  test("does send excludeHidden to search-actions, where the field is real", async () => {
    const base = fake();
    withInquiryActions(base.fake);
    await buildCorpus(base.host, true);
    const call = base.fake.refsCalled("/builtin/action/search-actions")[0];
    expect(call.params.excludeHidden).toBe(true);
  });
});

describe("renderCorpus", () => {
  test("says so plainly when the store lists nothing", () => {
    const text = renderCorpus({ paths: [], types: [], actions: [] });
    expect(text).toContain("none listed");
  });

  test("does not offer the package's own transcripts as a place to look", () => {
    const text = renderCorpus({
      paths: [{ path: "/inquiry/sessions", count: 9 }, { path: "/blog", count: 2 }],
      types: [],
      actions: [],
    });
    expect(text).toContain("/blog");
    expect(text).not.toContain("/inquiry");
  });

  test("is capped, so a huge store cannot flood the planner", () => {
    const paths = Array.from({ length: 500 }, (_, i) => ({ path: "/p" + i, count: i }));
    const text = renderCorpus({ paths, types: [], actions: [] });
    expect(text.length).toBeLessThan(3000);
    expect(text).toContain("truncated");
  });
});

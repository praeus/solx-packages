/** Catalogue resolution: the wire keys, the host-side exclusion, the cap. */

import { describe, expect, test } from "vitest";
import { resolveCatalogue, normalizeSchema, encodeToolName } from "../src/harness/catalogue";
import { DOCS_GRANT, fake, withDocActions } from "./fakeHost";

function seeded() {
  const { fake: f, host } = fake();
  withDocActions(f).action("/builtin/document/set_field_at_path", {
    description: "set one document field",
  });
  return { f, host };
}

describe("resolveCatalogue", () => {
  test("sends the keys solx actually reads", async () => {
    // Regression: ActionSearchQuery is rename_all = "camelCase", and schemas
    // are open, so path_prefix/exclude_hidden are dropped in silence -- an
    // unfiltered catalogue with no error anywhere.
    const { f, host } = seeded();
    await resolveCatalogue(host, "documents", DOCS_GRANT, 10, null);
    const [search] = f.refsCalled("/builtin/action/search_actions");
    expect(Object.keys(search.params).sort()).toEqual([
      "excludeHidden",
      "limit",
      "pathPrefix",
      "q",
    ]);
    expect(search.params.excludeHidden).toBe(true);
    expect(search.params.pathPrefix).toBe("/builtin/document");
  });

  test("hidden actions never enter the catalogue", async () => {
    const { f, host } = seeded();
    f.action("/builtin/document/secret_doc_thing", {
      description: "document",
      capabilities: ["solx:hidden"],
    });
    const cat = await resolveCatalogue(host, "document", DOCS_GRANT, 10, null);
    expect(Object.values(cat.map)).not.toContain("/builtin/document/secret_doc_thing");
    expect(Object.values(cat.map)).toContain("/builtin/document/search_documents");
  });

  test("the cap truncates and reports what it dropped", async () => {
    const { host } = seeded();
    const cat = await resolveCatalogue(host, "document", DOCS_GRANT, 2, null);
    expect(cat.tools.length).toBe(2);
    expect(cat.dropped).toBe(2);
  });

  test("a command action surfaces under a plain path grant, with no exact name needed", async () => {
    // resolveCatalogue used to filter through a stricter predicate here,
    // requiring grant[].actions to name a Command/Webhook row exactly -- a
    // wide grant hid it from discovery entirely, not just from dispatch.
    // gate.test.ts covers the predicate directly; this is the effect that
    // actually matters, one layer up.
    const { f, host } = seeded();
    f.action("/tools/build", { actionType: "command", description: "build a thing" });
    const cat = await resolveCatalogue(host, "build", [{ path: "/tools" }], 10, null);
    expect(Object.values(cat.map)).toContain("/tools/build");
  });

  test("a `*` grant resolves the whole catalogue", async () => {
    // Regression: `pathPrefix` is a SQL prefix match, so passing a pattern
    // through it looked for a path literally named `/*` and matched nothing.
    // The gate would have allowed every row; none ever reached it, so the
    // widest possible grant resolved to an empty catalogue and the panel
    // advised widening it further.
    const { f, host } = seeded();
    f.action("/tools/build", { description: "build a thing" });
    const cat = await resolveCatalogue(host, null, [{ path: "*" }], 10, null);
    expect(Object.values(cat.map)).toContain("/tools/build");
    expect(Object.values(cat.map)).toContain("/builtin/document/search_documents");
    const [search] = f.refsCalled("/builtin/action/search_actions");
    expect("pathPrefix" in search.params).toBe(false);
  });

  test("a narrower pattern still narrows", async () => {
    // The unscoped search is only how rows are *fetched* -- `permitted` still
    // applies the pattern, so `/builtin/*` must not drag in /tools.
    const { f, host } = seeded();
    f.action("/tools/build", { description: "build a thing" });
    const cat = await resolveCatalogue(host, null, [{ path: "/builtin/*" }], 10, null);
    expect(Object.values(cat.map)).toContain("/builtin/document/search_documents");
    expect(Object.values(cat.map)).not.toContain("/tools/build");
  });

  test("known refs are not re-offered", async () => {
    const { host } = seeded();
    const known = { "/builtin/document/search_documents": true };
    const cat = await resolveCatalogue(host, "document", DOCS_GRANT, 10, known);
    expect(Object.values(cat.map)).not.toContain("/builtin/document/search_documents");
  });

  test("a tool name is readable, and resolved through the map rather than parsed", async () => {
    expect(encodeToolName("/builtin/document", "search_documents")).toBe(
      "act__builtin__document__search_documents",
    );
    const { host } = seeded();
    const cat = await resolveCatalogue(host, "document", DOCS_GRANT, 10, null);
    expect(cat.map["act__builtin__document__search_documents"]).toBe(
      "/builtin/document/search_documents",
    );
  });
});

describe("schema normalization", () => {
  test("flattens null unions, because small models emit the string 'null'", () => {
    const out = normalizeSchema({
      type: "object",
      properties: {
        a: { type: ["string", "null"] },
        b: { type: "object", properties: { c: { type: ["integer", "null"] } } },
        d: { type: "array", items: { type: ["string", "null"] } },
      },
    }) as Record<string, Record<string, Record<string, unknown>>>;
    expect(out.properties.a.type).toBe("string");
    expect((out.properties.b.properties as Record<string, Record<string, unknown>>).c.type).toBe(
      "integer",
    );
    expect((out.properties.d.items as Record<string, unknown>).type).toBe("string");
  });

  test("is always an object schema, because Ollama requires one", () => {
    expect(normalizeSchema(null)).toEqual({ type: "object", properties: {} });
    expect(normalizeSchema({ type: "string" })).toEqual({ type: "object", properties: {} });
    expect(normalizeSchema({ type: "object" })).toEqual({ type: "object", properties: {} });
  });
});

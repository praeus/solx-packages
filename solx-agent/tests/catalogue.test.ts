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

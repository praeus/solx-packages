import { exec } from "sol:actions/action-exec@0.1.0";

// componentize-qjs binds the WIT `runner` interface to a named export
// matching its identifier, not a bare top-level `run` function — exporting
// `run` directly fails at runtime with "interface 'sol:actions/runner@0.1.0'
// not found". See README.md.
export const runner = {
  run(actionName, params) {
    const input = JSON.parse(params || "{}");
    const listResult = exec("/builtin/entity_list_documents", JSON.stringify({}));
    if (!listResult.success) {
      return { success: false, message: listResult.message || "list documents failed", output: null };
    }

    const documents = JSON.parse(listResult.output || "{}").items || [];
    const summary = documents.slice(0, input.limit || 10).map((doc) => {
      const getResult = exec("/builtin/entity_get_document", JSON.stringify({ path: doc.path, name: doc.name }));
      if (!getResult.success) {
        return { name: doc.name, firstLine: null, error: getResult.message || "get document failed" };
      }

      const document = JSON.parse(getResult.output || "{}") || {};
      const contents = document.contents || {};
      const text = (contents.normalized_text || contents.extracted_text || contents.text || contents.body || "")
        .toString()
        .split(/\r?\n/)[0] || "";

      return { name: doc.name, firstLine: text };
    });

    return { success: true, message: null, output: JSON.stringify(summary) };
  }
};

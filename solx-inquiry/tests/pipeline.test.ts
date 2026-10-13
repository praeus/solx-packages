/**
 * The pipeline end to end, against a scripted model and the fake host.
 *
 * This is where the two design claims are actually checked: that a plan is
 * validated rather than trusted, and that the answer is grounded in what the
 * lookups returned rather than in the model's memory. The fake replies are
 * consumed in call order, so a test that queues a plan then an answer is
 * asserting the *order* of the two model calls as much as their content.
 *
 * The `format` assertion matters on its own: if the plan call ever stopped
 * sending a schema, a small model would answer in prose and the pipeline would
 * silently get `{steps: []}` instead of a plan.
 */

import { describe, expect, test } from "vitest";
import { createInquiry } from "../src/harness/agent";
import { runInquiry } from "../src/harness/pipeline";
import { fake, withInquiryActions, type FakeHost } from "./fakeHost";
import type { Host, InquirySession } from "../src/harness";

let seq = 0;

async function start(model = "qwen3:4b"): Promise<{ f: FakeHost; host: Host; session: InquirySession }> {
  const base = fake();
  withInquiryActions(base.fake)
    .doc("/blog/rust-1", {
      title: "Async Rust",
      summary: "getting started with async",
      contents: { body: "Rust async is powered by futures." },
    })
    .doc("/blog/rust-2", {
      title: "Rust traits",
      summary: "traits and generics",
      contents: { body: "Traits describe shared behaviour." },
    })
    .doc("/research/paper-a", { title: "A paper", summary: "unrelated", contents: { body: "science" } });
  // A unique id per test, so two tests never collide on a saved document.
  const session = await createInquiry(base.host, "Summarize the articles", { model, title: "t" + seq++ });
  return { f: base.fake, host: base.host, session };
}

const noop = { onProgress: () => {} };

describe("runInquiry", () => {
  test("asks the planner with a schema, then answers from the documents it gathered", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "rust", path_prefix: "/blog", limit: 5 }] });
    f.replyText("Rust async is built on futures [/blog/rust-1].");

    const result = await runInquiry(host, session, "How does Rust async work?", noop);

    expect(result.status).toBe("idle");
    expect(result.answer).toContain("futures");
    expect(result.documents.map((d) => d.ref)).toContain("/blog/rust-1");
  });

  test("the plan call carries a JSON schema in `format` and no tools", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "rust" }] });
    f.replyText("answer");
    await runInquiry(host, session, "q", noop);

    // The first chat is the plan: it carries a `format` schema and no tools.
    expect(f.chatParams[0]?.format).toBeTruthy();
    expect(f.chatParams[0]?.tools).toBeUndefined();
    // Thinking off for the plan only -- see `requestPlan`.
    expect(f.chatParams[0]?.think).toBe(false);
    expect(f.chatParams[1]?.think).toBeUndefined();
  });

  test("the answer call carries neither `format` nor `tools`", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "rust" }] });
    f.replyText("answer");
    await runInquiry(host, session, "q", noop);

    expect(f.chatParams).toHaveLength(2);
    expect(f.chatParams[1]?.format).toBeUndefined();
    expect(f.chatParams[1]?.tools).toBeUndefined();
  });

  test("grounds the answer: the documents sent to the writer contain what was found", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "traits", path_prefix: "/blog" }] });
    f.replyText("Traits describe shared behaviour.");

    await runInquiry(host, session, "What are traits?", noop);

    // The answer prompt is the *last* chat the pipeline sent; assert its user
    // message carries the document body, not just the question.
    const messages = (f.lastChatParams?.messages ?? []) as { role: string; content: string }[];
    const user = messages.find((m) => m.role === "user");
    expect(user?.content).toContain("Rust traits");
    expect(user?.content).toContain("Traits describe shared behaviour.");
  });

  test("an empty plan with no direct answer triggers a replan rather than giving up", async () => {
    // Found by running the package: a model returned `{}` for a clear question,
    // and the turn answered "I could not find that" without ever searching.
    const { f, host, session } = await start();
    f.replyPlan({ steps: [] }); // planner failed to propose anything
    f.replyPlan({ steps: [{ verb: "search", q: "rust", path_prefix: "/blog" }] }); // the retry works
    f.replyText("Found it on the retry.");

    const result = await runInquiry(host, session, "anything about rust?", noop);
    expect(result.status).toBe("idle");
    expect(result.documents.length).toBeGreaterThan(0);
    expect(result.answer).toContain("retry");
  });

  test("does not replan an empty plan when replan is off", async () => {
    const { f, host, session } = await start();
    session.options.replan = false;
    f.replyPlan({ steps: [] });
    f.replyText("I could not find that.");
    const result = await runInquiry(host, session, "q", noop);
    expect(result.documents).toHaveLength(0);
    expect(f.chatParams).toHaveLength(2); // plan + answer, no retry
  });

  test("a `direct` answer is never retried, even with no documents", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [], direct: "Ask me about your documents." });
    const result = await runInquiry(host, session, "hello", noop);
    expect(result.answer).toBe("Ask me about your documents.");
    // Exactly one chat: the plan. The retry must not fire for a deliberate
    // decision to answer without looking anything up.
    expect(f.chatParams).toHaveLength(1);
  });

  test("replans once when the first plan finds nothing", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "zzznomatch" }] });   // finds nothing
    f.replyPlan({ steps: [{ verb: "search", q: "rust", path_prefix: "/blog" }] }); // finds some
    f.replyText("Found it.");

    const result = await runInquiry(host, session, "anything about rust?", noop);
    expect(result.status).toBe("idle");
    expect(result.documents.length).toBeGreaterThan(0);
    // The transcript shows the retry's plan, because that is what produced the docs.
    expect(result.plan[0]).toMatchObject({ verb: "search", q: "rust" });
  });

  test("does not replan when disabled", async () => {
    const { f, host, session } = await start();
    session.options.replan = false;
    f.replyPlan({ steps: [{ verb: "search", q: "zzznomatch" }] });
    f.replyText("I could not find anything.");

    const result = await runInquiry(host, session, "q", noop);
    expect(result.status).toBe("idle");
    expect(result.documents).toHaveLength(0);
    expect(result.answer).toContain("could not find");
  });

  test("an unanswerable question still yields an honest answer", async () => {
    const { f, host, session } = await start();
    session.options.replan = false;
    f.replyPlan({ steps: [{ verb: "search", q: "nothinghere" }] });
    f.replyText("I could not find that. Try naming a path.");

    const result = await runInquiry(host, session, "what colour is the sky?", noop);
    expect(result.answer).toContain("could not find");
    expect(result.documents).toHaveLength(0);
  });

  test("a planner that wrote prose is replanned, never returned as the answer", async () => {
    const { f, host, session } = await start();
    // The model ignored `format`. Its prose is not grounded in anything, so it
    // must not become the answer -- it is an empty plan, and gets the retry.
    f.replyText("I think you want the blog posts.");
    f.replyPlan({ steps: [{ verb: "search", q: "rust", path_prefix: "/blog" }] });
    f.replyText("From the documents: futures [/blog/rust-1].");
    const result = await runInquiry(host, session, "q", noop);
    expect(session.status).toBe("idle");
    expect(f.chatParams).toHaveLength(3);
    expect(result.answer).not.toContain("I think you want");
    expect(result.documents.length).toBeGreaterThan(0);
  });

  test("a plan wrapped in a code fence is still read as a plan", async () => {
    const { f, host, session } = await start();
    f.replyText('```json\n{"steps":[{"verb":"search","q":"traits"}]}\n```');
    f.replyText("answer");
    const result = await runInquiry(host, session, "q", noop);
    expect(result.plan[0]?.q).toBe("traits");
    expect(f.chatParams).toHaveLength(2);
  });

  test("persists the turn, so the document is the record", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "rust", path_prefix: "/blog" }] });
    f.replyText("An answer.");
    await runInquiry(host, session, "q", noop);

    const stored = f.inquiry(session.id) as { turns?: unknown[]; answer?: string } | undefined;
    expect(stored?.turns).toHaveLength(1);
    expect(stored?.answer).toBe("An answer.");
  });

  test("reports progress after each phase, in order", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "rust" }] });
    f.replyText("answer");
    const seen: string[] = [];
    await runInquiry(host, session, "q", {
      onProgress: (r) => seen.push(r.status),
    });
    expect(seen).toContain("planning");
    expect(seen).toContain("gathering");
    expect(seen).toContain("answering");
    expect(seen[seen.length - 1]).toBe("idle");
  });

  test("an answer call that returns nothing does not discard the gathered documents", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "traits", path_prefix: "/blog" }] });
    f.replyText(""); // the writer returned empty
    const result = await runInquiry(host, session, "q", noop);
    expect(result.documents.length).toBeGreaterThan(0);
    expect(result.answer).toBeTruthy();
    expect(result.answer).toContain("no answer");
  });

  test("a turn abandoned mid-phase settles instead of being left live", async () => {
    // This is the bug running the package found: an abandoned `await` never
    // reaches the catch, so without an explicit check the document stays
    // `planning` forever and a later read reports a state that is not true.
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "rust" }] });
    f.replyText("answer");
    let abandoned = false;
    const result = await runInquiry(
      host,
      session,
      "q",
      {
        onProgress: (r) => {
          // Abandon as soon as gathering starts -- the next phase boundary
          // must notice and settle.
          if (r.status === "gathering") abandoned = true;
        },
      },
      { isAbandoned: () => abandoned },
    );
    expect(result.status).toBe("error");
    expect(session.status).toBe("error");
    expect(session.turns[session.turns.length - 1].status).toBe("error");
  });

  test("a Stop during planning runs no lookups", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "rust" }] });
    f.replyText("answer");
    let abandoned = false;
    const result = await runInquiry(
      host,
      session,
      "q",
      { onProgress: (r) => { if (r.status === "planning") abandoned = true; } },
      { isAbandoned: () => abandoned },
    );
    expect(result.status).toBe("error");
    expect(result.error).toBe("stopped");
    // The corpus samples documents before planning; a lookup is a term search.
    expect(f.calls.filter((c) => c.ref.endsWith("/search-documents") && c.params.q)).toHaveLength(0);
  });

  test("a stopped run settling late does not mark a newer turn finished", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [{ verb: "search", q: "rust" }] });
    f.replyText("answer");
    let abandoned = false;
    const result = await runInquiry(
      host,
      session,
      "first",
      {
        onProgress: (r) => {
          if (r.status !== "gathering") return;
          // The person stopped and asked again; the next turn is now live on
          // the same session object while this one is still unwinding.
          abandoned = true;
          session.turns.push({
            question: "second", status: "planning", plan: [], plan_error: null,
            corpus: "", steps: [], documents: [], answer: null,
          });
          session.status = "planning";
        },
      },
      { isAbandoned: () => abandoned },
    );
    expect(result.error).toBe("stopped");
    expect(session.turns[0].status).toBe("error");
    expect(session.status).toBe("planning");
  });

  test("a phase that throws settles the turn as an error, never live", async () => {
    const { host, session } = await start();
    // No scripted reply: the planner call fails.
    const result = await runInquiry(host, session, "q", noop);
    expect(result.status).toBe("error");
    expect(session.status).toBe("error");
    expect(result.error).toBeTruthy();
  });
});

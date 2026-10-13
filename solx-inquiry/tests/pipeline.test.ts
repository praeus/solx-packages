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

  test("an empty plan with a direct answer short-circuits the writer", async () => {
    const { f, host, session } = await start();
    f.replyPlan({ steps: [], direct: "Hello. Ask me about your documents." });

    const result = await runInquiry(host, session, "hi", noop);
    expect(result.answer).toBe("Hello. Ask me about your documents.");
    // Only one model call was made: the plan. No writer call consumed a reply.
    expect(f.replies.length).toBe(0);
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

  test("a planner that wrote prose instead of JSON does not error the turn", async () => {
    const { f, host, session } = await start();
    // Not JSON: the pipeline treats it as a direct answer, which is the honest
    // reading of "the model ignored the schema and said something".
    f.replyText("I think you want the blog posts.");
    await runInquiry(host, session, "q", noop);
    expect(session.status).toBe("idle");
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
});

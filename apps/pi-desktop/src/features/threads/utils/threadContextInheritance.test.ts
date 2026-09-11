import { describe, expect, it } from "vitest";
import { contextInheritanceFromThread } from "./threadContextInheritance";

const origin = {
  mode: "fork", parentThreadId: "parent", parentEntryId: "cutoff", snapshotEntryId: "seed",
};
const turns = [{ id: "inherited-turn", items: [
  { id: "parent-user", type: "userMessage", content: [{ type: "text", text: "Parent request" }, { type: "image", url: "data:image/png;base64,AAA" }] },
  { id: "parent-reasoning", type: "reasoning", content: ["Parent thought"] },
  { id: "parent-tool", type: "mcpToolCall", server: "pi", tool: "read", status: "completed", result: "File content" },
  { id: "parent-answer", type: "agentMessage", text: "Parent answer" },
] }];

describe("contextInheritanceFromThread", () => {
  it("converts only inherited turns, keeping their identities and images separate from child messages", () => {
    const result = contextInheritanceFromThread({
      contextOrigin: origin, inheritedContext: { turns },
      turns: [{ items: [{ id: "child-answer", type: "agentMessage", text: "Child answer" }] }],
    });
    expect(result?.origin).toEqual(origin);
    expect(result?.origin).not.toBe(origin);
    expect(result?.inheritedItems).toEqual([
      expect.objectContaining({ id: "parent-user", text: "Parent request", images: ["data:image/png;base64,AAA"] }),
      expect.objectContaining({ id: "parent-reasoning", content: "Parent thought" }),
      expect.objectContaining({ id: "parent-tool", output: "File content" }),
      expect.objectContaining({ id: "parent-answer", text: "Parent answer" }),
    ]);
  });

  it("never invents inheritance from parent links, spawn content or existing messages", () => {
    expect(contextInheritanceFromThread({ parentThreadId: "parent", source: { subAgent: { parentThreadId: "parent" } }, inheritedContext: { turns }, turns })).toBeNull();
  });

  it("keeps fresh children explicit and ignores any supplied inherited history", () => {
    const fresh = { ...origin, mode: "fresh", parentEntryId: null, snapshotEntryId: null };
    expect(contextInheritanceFromThread({ contextOrigin: fresh, inheritedContext: { turns } })).toEqual({ origin: fresh, inheritedItems: null });
  });

  it("distinguishes a known empty snapshot from an unavailable snapshot", () => {
    expect(contextInheritanceFromThread({ contextOrigin: origin, inheritedContext: { turns: [] } })?.inheritedItems).toEqual([]);
    expect(contextInheritanceFromThread({ contextOrigin: origin })?.inheritedItems).toBeNull();
  });

  it.each([
    null, [], "fork", {}, { ...origin, mode: "unknown" }, { ...origin, parentThreadId: " " },
    { ...origin, parentThreadId: 123 }, { ...origin, parentEntryId: undefined },
    { ...origin, snapshotEntryId: undefined }, { ...origin, snapshotEntryId: [] },
  ])("rejects invalid origin metadata: %j", (contextOrigin) => {
    expect(contextInheritanceFromThread({ contextOrigin, inheritedContext: { turns } })).toBeNull();
  });

  it.each([
    null, [], {}, { turns: null }, { turns: [null] }, { turns: [{ items: [null] }] },
    { turns: [{ items: null }] }, { turns: [{ items: [42] }] },
  ])("treats malformed inherited envelopes as unavailable: %j", (inheritedContext) => {
    expect(contextInheritanceFromThread({ contextOrigin: origin, inheritedContext })).toEqual({ origin, inheritedItems: null });
  });

  it("leaves item variant validation to the shared converter without rejecting the whole snapshot", () => {
    const inheritedContext = { turns: [{ items: [
      { type: "agentMessage" },
      { id: "future", type: "futureItem", extension: true },
      { id: "answer", type: "agentMessage", text: "Supported content" },
    ] }] };
    expect(contextInheritanceFromThread({ contextOrigin: origin, inheritedContext })?.inheritedItems).toEqual([
      expect.objectContaining({ id: "answer", text: "Supported content" }),
    ]);
  });

  it("marks an unconvertible snapshot unavailable without leaking a conversion failure", () => {
    const inheritedContext = { turns: [{ items: [{ id: "bad", type: "userMessage", content: [null] }] }] };
    expect(contextInheritanceFromThread({ contextOrigin: origin, inheritedContext })).toEqual({ origin, inheritedItems: null });
  });
});

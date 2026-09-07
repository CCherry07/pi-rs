import { describe, expect, it } from "vitest";
import type { ConversationItem } from "../../../types";
import {
  buildToolGroups,
  buildToolSummary,
  statusToneFromText,
} from "./messageRenderUtils";

function makeToolItem(
  overrides: Partial<Extract<ConversationItem, { kind: "tool" }>>,
): Extract<ConversationItem, { kind: "tool" }> {
  return {
    id: "tool-1",
    kind: "tool",
    toolType: "webSearch",
    title: "Web search",
    detail: "codex monitor",
    status: "completed",
    output: "",
    ...overrides,
  };
}

describe("messageRenderUtils", () => {
  it("renders web search as searching while in progress", () => {
    const summary = buildToolSummary(makeToolItem({ status: "inProgress" }), "");
    expect(summary.label).toBe("searching");
    expect(summary.value).toBe("codex monitor");
  });

  it("renders mcp search calls as searching while in progress", () => {
    const summary = buildToolSummary(
      makeToolItem({
        toolType: "mcpToolCall",
        title: "Tool: web / search_query",
        detail: '{\n  "query": "codex monitor"\n}',
        status: "inProgress",
      }),
      "",
    );
    expect(summary.label).toBe("searching");
    expect(summary.value).toBe("codex monitor");
  });

  it("parses localized Pi tool titles without repeating the generic label", () => {
    const readSummary = buildToolSummary(
      makeToolItem({
        toolType: "mcpToolCall",
        title: "工具：pi / read",
        detail: '{"path":"/repo/src/lib.rs"}',
      }),
      "",
    );
    const findSummary = buildToolSummary(
      makeToolItem({
        toolType: "mcpToolCall",
        title: "工具：pi / find",
        detail: "{}",
      }),
      "",
    );

    expect(readSummary).toMatchObject({
      label: "read",
      value: "lib.rs",
      detail: "/repo/src/lib.rs",
    });
    expect(findSummary).toMatchObject({ label: "", value: "pi / find" });
  });

  it("classifies camelCase inProgress as processing", () => {
    expect(statusToneFromText("inProgress")).toBe("processing");
  });

  it("renders collab tool calls with nickname and role", () => {
    const summary = buildToolSummary(
      makeToolItem({
        toolType: "collabToolCall",
        title: "Collab: wait",
        detail: "From thread-parent → thread-child",
        status: "completed",
        output: "Robie [explorer]: completed",
        collabReceivers: [
          {
            threadId: "thread-child",
            nickname: "Robie",
            role: "explorer",
          },
        ],
      }),
      "",
    );
    expect(summary.label).toBe("waited for");
    expect(summary.value).toBe("Robie [explorer]");
    expect(summary.output).toContain("Robie [explorer]: completed");
  });

  it("keeps sub-agent execution trees outside collapsed tool groups", () => {
    const entries = buildToolGroups([
      makeToolItem({ id: "read-1", toolType: "mcpToolCall" }),
      makeToolItem({
        id: "subagent-1",
        toolType: "collabToolCall",
        title: "Collab: spawn",
      }),
      makeToolItem({ id: "read-2", toolType: "mcpToolCall" }),
    ]);

    expect(entries.map((entry) => entry.kind)).toEqual(["item", "item", "item"]);
    expect(entries[1]).toMatchObject({
      kind: "item",
      item: { id: "subagent-1", toolType: "collabToolCall" },
    });
  });
});

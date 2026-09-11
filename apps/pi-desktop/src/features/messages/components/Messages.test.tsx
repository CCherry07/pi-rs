// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConversationItem } from "@/types";
import { ThreadConversationsContext, type ThreadConversationsSource } from "@threads/contexts/ThreadConversations";
import { Messages } from "./Messages";

vi.mock("../hooks/useFileLinkOpener", () => ({
  useFileLinkOpener: () => ({ openFileLink: vi.fn(), showFileLinkMenu: vi.fn() }),
}));

const spawn: ConversationItem = {
  id: "spawn-call", kind: "tool", toolType: "collabToolCall", title: "Collab: spawn",
  detail: "", status: "completed", collabTask: "Review the parser",
  collabReceiver: { threadId: "child", role: "reviewer" },
  collabStatuses: [{ threadId: "child", role: "reviewer", status: "completed", totalTokens: 321 }],
};
const childItems: ConversationItem[] = [
  { id: "request", kind: "message", role: "user", text: "Check parser edge cases" },
  { id: "thought", kind: "reasoning", summary: "Checking syntax", content: "Inspecting parser branches" },
  { id: "shell", kind: "tool", toolType: "commandExecution", title: "Command: cargo test parser", detail: "/project", status: "completed", output: "All parser tests passed" },
  { id: "answer", entryId: "saved-answer", kind: "message", role: "assistant", text: "**Parser review complete**" },
];
const rootItems: ConversationItem[] = [
  { id: "parent-user", kind: "message", role: "user", text: "Parent request" },
  spawn,
  { id: "parent-answer", kind: "message", role: "assistant", text: "Parent answer" },
];
function source(overrides: Partial<ThreadConversationsSource> = {}): ThreadConversationsSource {
  return {
    itemsByThread: { child: childItems },
    threadStatusById: { child: { isProcessing: false, hasUnread: false, processingStartedAt: null, lastDurationMs: null } },
    tokenUsageByThread: {},
    loadThread: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}
function chat(value: ThreadConversationsSource, items = rootItems, onOpenThreadLink = vi.fn(), onForkMessage = vi.fn()) {
  return (
    <ThreadConversationsContext.Provider value={value}>
      <Messages items={items} threadId="parent" workspaceId="workspace" workspacePath="/project" isThinking={false}
        openTargets={[]} selectedOpenAppId="" onOpenThreadLink={onOpenThreadLink} onForkMessage={onForkMessage} />
    </ThreadConversationsContext.Provider>
  );
}

afterEach(cleanup);

describe("embedded child-agent conversations", () => {
  it("keeps inherited history separate and opens a frozen, read-only snapshot", async () => {
    const inheritedItems: ConversationItem[] = [
      { id: "seed-user", kind: "message", role: "user", text: "Parent context at creation" },
      { ...spawn, id: "seed-spawn", collabReceiver: { threadId: "old-agent", role: "scout" } },
    ];
    const value = source({ contextInheritanceByThread: { child: {
      origin: { mode: "fork", parentThreadId: "parent", parentEntryId: "entry-before-spawn", snapshotEntryId: "seed-entry" },
      inheritedItems,
    } } });
    const navigate = vi.fn();
    const fork = vi.fn();
    const { rerender } = render(chat(value, rootItems, navigate, fork));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledOnce());
    const panel = screen.getByRole("region", { name: "reviewer conversation" });
    expect(within(panel).getByText("Inherited the parent context at creation")).toBeTruthy();
    expect(within(panel).getByText("entry-before-spawn")).toBeTruthy();
    expect(within(panel).queryByText("Parent context at creation")).toBeNull();
    expect(within(panel).queryByText("Parent request")).toBeNull();
    expect(within(panel).getByText("Parser review complete")).toBeTruthy();
    fireEvent.click(within(panel).getByRole("button", { name: "View inherited context" }));
    const snapshot = within(panel).getByRole("region", { name: "Inherited context snapshot" });
    expect(within(snapshot).getByText("Parent context at creation")).toBeTruthy();
    expect(within(snapshot).queryByText("Parser review complete")).toBeNull();
    expect(within(snapshot).queryByRole("button", { name: "Fork from this message" })).toBeNull();
    expect(within(snapshot).queryByRole("button", { name: "Expand child-agent chat" })).toBeNull();
    fireEvent.click(within(snapshot).getByRole("button", { name: "Toggle tool details" }));
    expect(value.loadThread).toHaveBeenCalledOnce();

    rerender(chat({ ...value, itemsByThread: { ...value.itemsByThread, parent: [
      ...rootItems, { id: "later", kind: "message", role: "user", text: "Later parent message" },
    ] } }, rootItems, navigate, fork));
    expect(within(snapshot).queryByText("Later parent message")).toBeNull();
    expect(navigate).not.toHaveBeenCalled();
    expect(fork).not.toHaveBeenCalled();
    const controlIds = Array.from(document.querySelectorAll("[aria-controls]"), (node) => node.getAttribute("aria-controls"));
    expect(new Set(controlIds).size).toBe(controlIds.length);
    fireEvent.click(within(panel).getByRole("button", { name: "Hide inherited context" }));
    expect(within(panel).queryByText("Parent context at creation")).toBeNull();
    expect(within(panel).getByText("Parser review complete")).toBeTruthy();
  });

  it("labels fresh children without offering a parent-history snapshot", async () => {
    const value = source({ contextInheritanceByThread: { child: {
      origin: { mode: "fresh", parentThreadId: "parent", parentEntryId: null, snapshotEntryId: null },
      inheritedItems: null,
    } } });
    render(chat(value));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledOnce());
    expect(screen.getByText("Task-only context; no parent history inherited")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "View inherited context" })).toBeNull();
  });

  it("shows provenance in a directly displayed child and distinguishes unavailable snapshots", () => {
    const value = source({ contextInheritanceByThread: { child: {
      origin: { mode: "fork", parentThreadId: "parent", parentEntryId: null, snapshotEntryId: "legacy-seed" },
      inheritedItems: null,
    } } });
    render(<ThreadConversationsContext.Provider value={value}>
      <Messages items={childItems} threadId="child" workspaceId="workspace" isThinking={false}
        openTargets={[]} selectedOpenAppId="" />
    </ThreadConversationsContext.Provider>);
    fireEvent.click(screen.getByRole("button", { name: "View inherited context" }));
    expect(screen.getByText("The inherited snapshot is unavailable.")).toBeTruthy();
    expect(value.loadThread).not.toHaveBeenCalled();
  });

  it("expands real messages, reasoning and tools in the parent chat without navigation", async () => {
    const value = source();
    const navigate = vi.fn();
    const fork = vi.fn();
    render(chat(value, rootItems, navigate, fork));
    expect(value.loadThread).not.toHaveBeenCalled();
    expect(screen.queryByText("Parser review complete")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledExactlyOnceWith("workspace", "child"));
    const panel = screen.getByRole("region", { name: "reviewer conversation" });
    expect(within(panel).getByText("Check parser edge cases")).toBeTruthy();
    expect(within(panel).getByText("Parser review complete").tagName).toBe("STRONG");
    expect(within(panel).getByText("Checking syntax")).toBeTruthy();
    expect(within(panel).getByText("cargo test parser")).toBeTruthy();
    fireEvent.click(within(panel).getByRole("button", { name: "Toggle tool details" }));
    expect(within(panel).getByText("All parser tests passed")).toBeTruthy();
    expect(panel.closest(".tool-group")).toBeNull();
    expect(panel.querySelector(".messages-embedded")).toBeTruthy();
    expect(panel.querySelector(".messages-full")).toBeNull();
    expect(within(panel).queryByRole("button", { name: "Fork from this message" })).toBeNull();
    expect(screen.getByText("Parent request")).toBeTruthy();
    expect(screen.getByText("Parent answer")).toBeTruthy();
    expect(screen.getByText("idle · 0.3k tokens")).toBeTruthy();
    expect(navigate).not.toHaveBeenCalled();
    expect(fork).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Collapse child-agent chat" }));
    expect(screen.queryByText("Parser review complete")).toBeNull();
    expect(screen.getByText("Parent answer")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledTimes(2));
    expect(screen.getAllByText("Parser review complete")).toHaveLength(1);
  });

  it("updates the child transcript and activity while leaving the parent transcript untouched", async () => {
    const value = source();
    const { rerender } = render(chat(value));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledOnce());
    rerender(chat({
      ...value,
      itemsByThread: { child: [...childItems, { id: "next-answer", kind: "message", role: "assistant", text: "Live child update" }] },
      threadStatusById: { child: { isProcessing: true, hasUnread: false, processingStartedAt: null, lastDurationMs: null } },
    }));
    expect(screen.getByText("Live child update").closest(".messages-embedded")).toBeTruthy();
    expect(screen.getByText("processing · 0.3k tokens")).toBeTruthy();
    expect(screen.getAllByText("Parent answer")).toHaveLength(1);
  });

  it("shows loading and a retryable error instead of an empty child panel", async () => {
    let rejectLoad: (error: Error) => void = () => {};
    const loadThread = vi.fn().mockImplementationOnce(() => new Promise<void>((_, reject) => { rejectLoad = reject; }))
      .mockResolvedValue(undefined);
    render(chat(source({ itemsByThread: {}, loadThread })));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    expect(screen.getByText("Loading…")).toBeTruthy();
    await act(async () => { rejectLoad(new Error("Cannot read child session")); });
    expect(screen.getByRole("alert").textContent).toContain("Cannot read child session");
    expect(screen.queryByText("No child-agent messages yet.")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(loadThread).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(screen.getByText("No child-agent messages yet.")).toBeTruthy();
  });

  it("renders nested child panels and guards ancestor cycles without duplicate disclosure IDs", async () => {
    const value = source({ itemsByThread: {
      child: [
        ...childItems,
        { ...spawn, id: "nested-spawn", collabReceiver: { threadId: "grandchild", role: "scout" } },
      ],
      grandchild: [
        { id: "grandchild-answer", kind: "message", role: "assistant", text: "Grandchild findings" },
        { ...spawn, id: "cycle-spawn", collabReceiver: { threadId: "parent", role: "parent" } },
      ],
    } });
    render(chat(value));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledWith("workspace", "child"));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledWith("workspace", "grandchild"));
    expect(screen.getByText("Grandchild findings").closest(".messages-embedded")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    expect(screen.getByText("Child conversation is unavailable.")).toBeTruthy();
    expect(value.loadThread).toHaveBeenCalledTimes(2);
    const controlIds = Array.from(document.querySelectorAll("[aria-controls]"), (node) => node.getAttribute("aria-controls"));
    expect(new Set(controlIds).size).toBe(controlIds.length);
  });

  it("keeps siblings in distinct embedded conversations", async () => {
    const value = source({ itemsByThread: {
      child: childItems,
      sibling: [{ id: "sibling-answer", kind: "message", role: "assistant", text: "Sibling findings" }],
    } });
    render(chat(value, [{ ...spawn, collabReceivers: [
      { threadId: "child", role: "reviewer" }, { threadId: "sibling", role: "scout" },
    ] }]));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledTimes(2));
    expect(within(screen.getByRole("region", { name: "reviewer conversation" })).getByText("Parser review complete")).toBeTruthy();
    expect(within(screen.getByRole("region", { name: "scout conversation" })).getByText("Sibling findings")).toBeTruthy();
  });

  it("does not load an ancestor as its own child", async () => {
    const value = source();
    render(chat(value, [{ ...spawn, collabReceiver: { threadId: "parent", role: "reviewer" } }]));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    expect(screen.getByText("Child conversation is unavailable.")).toBeTruthy();
    expect(value.loadThread).not.toHaveBeenCalled();
  });

  it("shows child provider errors inside the conversation without hiding the parent", async () => {
    const value = source({ itemsByThread: { child: [{
      id: "error-1-0", kind: "tool", toolType: "notice", title: "Provider error", detail: "Provider unavailable", status: "failed",
    }] } });
    render(chat(value));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    await waitFor(() => expect(value.loadThread).toHaveBeenCalledOnce());
    const panel = screen.getByRole("region", { name: "reviewer conversation" });
    expect(within(panel).getByRole("alert").textContent).toContain("Provider unavailable");
    expect(screen.getByText("Parent answer")).toBeTruthy();
  });

  it("keeps a failed launch with no child identity visible", () => {
    render(chat(source(), [{ ...spawn, collabReceiver: undefined, status: "failed", output: "Agent profile was not found" }]));
    fireEvent.click(screen.getByRole("button", { name: "Expand child-agent chat" }));
    expect(screen.getByRole("alert").textContent).toContain("Agent profile was not found");
  });
});

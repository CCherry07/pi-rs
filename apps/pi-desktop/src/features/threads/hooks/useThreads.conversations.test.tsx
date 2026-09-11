// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PiEvent, WorkspaceInfo } from "@/types";
import { subscribePiEvents } from "@/services/events";
import { prepareThread, readThread, resumeThread, startThread } from "@services/tauri";
import { useThreads } from "./useThreads";

vi.mock("@/services/events", () => ({ subscribePiEvents: vi.fn(() => vi.fn()) }));
vi.mock("@services/tauri", () => ({
  prepareThread: vi.fn(), reloadThread: vi.fn(), startThread: vi.fn(),
  archiveThread: vi.fn(), readThread: vi.fn(), setThreadName: vi.fn(),
  forkThread: vi.fn(), listThreads: vi.fn(), listWorkspaces: vi.fn(), resumeThread: vi.fn(),
  interruptTurn: vi.fn(), sendUserMessage: vi.fn(), compactThread: vi.fn(),
}));

const workspace: WorkspaceInfo = { id: "workspace", name: "Workspace", path: "/project", settings: { sidebarCollapsed: false } };
const child = {
  id: "child", status: { type: "idle" }, activeTurnId: null,
  turns: [{ id: "child-turn", items: [{ id: "answer", type: "agentMessage", text: "Child history" }] }],
};

function emit(method: string, params: Record<string, unknown>) {
  const listener = vi.mocked(subscribePiEvents).mock.calls.slice(-1)[0][0];
  listener({ workspace_id: workspace.id, message: { method, params } } as PiEvent);
}

beforeEach(() => {
  vi.resetAllMocks();
  localStorage.clear();
  vi.mocked(subscribePiEvents).mockReturnValue(vi.fn());
  vi.mocked(prepareThread).mockResolvedValue({ thread: { id: "draft", commands: [] } });
  vi.mocked(startThread).mockResolvedValue({ thread: { id: "parent", commands: [], turns: [] } });
  vi.mocked(readThread).mockResolvedValue({ thread: child });
});
afterEach(cleanup);

describe("useThreads conversationSource", () => {
  const contextOrigin = { mode: "fork", parentThreadId: "parent", parentEntryId: "cutoff", snapshotEntryId: "seed" };
  const inheritedContext = { turns: [{ items: [{ id: "parent-before", type: "agentMessage", text: "Frozen parent answer" }] }] };

  it("exposes provenance from thread/started and replacement while keeping the parent selected", async () => {
    const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
    await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
    expect(result.current.conversationSource.contextInheritanceByThread?.parent).toBeNull();
    act(() => { emit("thread/started", { thread: { ...child, contextOrigin, inheritedContext } }); });
    expect(result.current.conversationSource.contextInheritanceByThread?.child).toEqual({
      origin: contextOrigin, inheritedItems: [expect.objectContaining({ text: "Frozen parent answer" })],
    });
    act(() => { emit("thread/replaced", { previousThreadId: "child", thread: { ...child, contextOrigin, inheritedContext } }); });
    expect(result.current.conversationSource.itemsByThread.child).toEqual([expect.objectContaining({ text: "Child history" })]);
    expect(result.current.activeThreadId).toBe("parent");
    act(() => { emit("thread/replaced", { previousThreadId: "child", thread: { ...child, turns: [] } }); });
    expect(result.current.conversationSource.contextInheritanceByThread?.child).toBeNull();
    expect(result.current.conversationSource.itemsByThread.child).toEqual([]);
  });

  it("does not restore old provenance when a pending child read resolves after replacement", async () => {
    let resolve!: (response: Record<string, unknown>) => void;
    vi.mocked(readThread).mockReturnValueOnce(new Promise((complete) => { resolve = complete; }));
    const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
    await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
    const pending = result.current.conversationSource.loadThread("workspace", "child");
    await act(async () => {
      emit("thread/replaced", { previousThreadId: "child", thread: { ...child, turns: [] } });
      resolve({ thread: { ...child, contextOrigin, inheritedContext } });
      await pending;
    });
    expect(result.current.conversationSource.contextInheritanceByThread?.child).toBeNull();
    expect(result.current.conversationSource.itemsByThread.child).toEqual([]);
  });

  it("hydrates and clears provenance through explicit resume without copying inherited items into history", async () => {
    const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
    await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
    vi.mocked(resumeThread).mockResolvedValueOnce({ thread: { ...child, contextOrigin, inheritedContext } });
    await act(async () => { await result.current.refreshThread("workspace", "child"); });
    expect(result.current.conversationSource.contextInheritanceByThread?.child?.origin).toEqual(contextOrigin);
    expect(result.current.conversationSource.itemsByThread.child).toEqual([expect.objectContaining({ text: "Child history" })]);
    vi.mocked(resumeThread).mockResolvedValueOnce({ thread: { ...child, turns: [] } });
    await act(async () => { await result.current.refreshThread("workspace", "child"); });
    expect(result.current.conversationSource.contextInheritanceByThread?.child).toBeNull();
    expect(result.current.conversationSource.itemsByThread.child).toEqual([]);
  });

  it("discards a pending resume after an authoritative same-ID replacement", async () => {
    let resolve!: (response: Record<string, unknown>) => void;
    vi.mocked(resumeThread).mockReturnValueOnce(new Promise((complete) => { resolve = complete; }));
    const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
    await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
    let pending!: Promise<string | null>;
    act(() => { pending = result.current.refreshThread("workspace", "child"); });
    await act(async () => {
      emit("thread/replaced", { previousThreadId: "child", thread: { ...child, turns: [] } });
      resolve({ thread: { ...child, contextOrigin, inheritedContext } });
      await pending;
    });
    expect(result.current.conversationSource.contextInheritanceByThread?.child).toBeNull();
    expect(result.current.conversationSource.itemsByThread.child).toEqual([]);
  });

  it("shares loaded child history while leaving the parent selection, items and session controls alone", async () => {
    const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
    await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
    const parentItems = result.current.activeItems;
    const loadThread = result.current.conversationSource.loadThread;
    await act(async () => { await loadThread("workspace", "child"); });
    expect(result.current.activeThreadId).toBe("parent");
    expect(result.current.activeItems).toEqual(parentItems);
    expect(result.current.conversationSource.itemsByThread.child).toEqual([
      expect.objectContaining({ id: "answer", text: "Child history" }),
    ]);
    expect(result.current.conversationSource.loadThread).toBe(loadThread);
    expect(readThread).toHaveBeenCalledExactlyOnceWith("workspace", "child");
    expect(startThread).toHaveBeenCalledTimes(1);
    expect(resumeThread).not.toHaveBeenCalled();
    expect(result.current.threadsByWorkspace.workspace.map((thread) => thread.id)).toEqual(["parent"]);
  });

  it("uses the same shared reducer for child streaming and follow-up turns", async () => {
    const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
    await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
    await act(async () => { await result.current.conversationSource.loadThread("workspace", "child"); });
    act(() => {
      emit("turn/started", { threadId: "child", turn: { id: "followup", status: "inProgress" } });
      emit("item/agentMessage/delta", { threadId: "child", itemId: "followup-answer", delta: "Live reply" });
      emit("thread/tokenUsage/updated", { threadId: "child", tokenUsage: { totalTokens: 450 } });
    });
    expect(result.current.conversationSource.itemsByThread.child).toEqual([
      expect.objectContaining({ text: "Child history" }),
      expect.objectContaining({ text: "Live reply" }),
    ]);
    expect(result.current.conversationSource.threadStatusById.child.isProcessing).toBe(true);
    expect(result.current.conversationSource.tokenUsageByThread.child.totalTokens).toBe(450);
    expect(result.current.activeThreadId).toBe("parent");
  });

  it("hydrates replacement usage in event order and lets later live usage win", async () => {
    const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
    await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
    act(() => {
      emit("thread/tokenUsage/updated", { threadId: "child", tokenUsage: { totalTokens: 100, contextTokens: 90 } });
      emit("thread/replaced", {
        previousThreadId: "child",
        thread: { ...child, tokenUsage: { total_tokens: "300", context_tokens: "20", model_context_window: "2000" } },
      });
    });
    expect(result.current.conversationSource.tokenUsageByThread.child).toEqual({
      totalTokens: 300, contextTokens: 20, modelContextWindow: 2000,
    });
    act(() => {
      emit("thread/replaced", { previousThreadId: "child", thread: { ...child, tokenUsage: { totalTokens: 400 } } });
      emit("thread/tokenUsage/updated", { threadId: "child", tokenUsage: { totalTokens: 500, contextTokens: 40 } });
    });
    expect(result.current.conversationSource.tokenUsageByThread.child).toMatchObject({ totalTokens: 500, contextTokens: 40 });
    expect(result.current.activeThreadId).toBe("parent");
  });

  it("does not merge an old read back after a same-session thread/replaced event", async () => {
    let resolve!: (response: { thread: typeof child }) => void;
    vi.mocked(readThread).mockReturnValueOnce(new Promise((complete) => { resolve = complete; }));
    const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
    await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
    const pending = result.current.conversationSource.loadThread("workspace", "child");
    await act(async () => {
      emit("thread/replaced", {
        previousThreadId: "child",
        thread: { ...child, turns: [{ id: "replacement", items: [{ id: "new", type: "agentMessage", text: "After compaction" }] }] },
      });
      resolve({ thread: child });
      await pending;
    });
    await waitFor(() => expect(result.current.conversationSource.itemsByThread.child).toEqual([
      expect.objectContaining({ id: "new", text: "After compaction" }),
    ]));
    expect(result.current.activeThreadId).toBe("parent");
  });
});

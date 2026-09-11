// @vitest-environment jsdom
import { useReducer } from "react";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readThread } from "@services/tauri";
import type { ConversationItem } from "@/types";
import { initialState, threadReducer, type ThreadState } from "./useThreadsReducer";
import { useThreadStatus } from "./useThreadStatus";
import { useThreadConversationLoader } from "./useThreadConversationLoader";

vi.mock("@services/tauri", () => ({ readThread: vi.fn() }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function snapshot(items: Record<string, unknown>[] = [], overrides: Record<string, unknown> = {}) {
  return {
    thread: {
      id: "child",
      status: { type: "idle" },
      activeTurnId: null,
      tokenUsage: { totalTokens: 100, contextTokens: 40, modelContextWindow: 2000 },
      turns: [{ id: "turn", status: "completed", items }],
      ...overrides,
    },
  };
}

function useHarness(seed: Partial<ThreadState> = {}) {
  const [state, dispatch] = useReducer(threadReducer, {
    ...initialState,
    activeThreadIdByWorkspace: { workspace: "parent" },
    ...seed,
  });
  const status = useThreadStatus({ dispatch });
  const loader = useThreadConversationLoader({ state, dispatch, getStatusRevision: status.getStatusRevision });
  return { state, dispatch, ...status, ...loader };
}

beforeEach(() => vi.resetAllMocks());
afterEach(cleanup);

describe("useThreadConversationLoader", () => {
  it("hydrates fork provenance and a frozen snapshot separately from child items and live parent messages", async () => {
    const origin = { mode: "fork", parentThreadId: "parent", parentEntryId: "cutoff", snapshotEntryId: "seed" };
    vi.mocked(readThread).mockResolvedValue(snapshot([
      { id: "answer", type: "agentMessage", text: "Own answer" },
    ], {
      contextOrigin: origin,
      inheritedContext: { turns: [{ items: [{ id: "inherited", type: "agentMessage", text: "Parent at fork" }] }] },
    }));
    const { result } = renderHook(() => useHarness({ itemsByThread: {
      parent: [{ id: "parent-later", kind: "message", role: "assistant", text: "Parent later" }],
    } }));
    await act(async () => { await result.current.loadThread("workspace", "child"); });
    expect(result.current.state.itemsByThread.child).toEqual([expect.objectContaining({ text: "Own answer" })]);
    expect(result.current.state.contextInheritanceByThread.child).toEqual({
      origin, inheritedItems: [expect.objectContaining({ id: "inherited", text: "Parent at fork" })],
    });
    act(() => {
      result.current.dispatch({ type: "appendAgentDelta", workspaceId: "workspace", threadId: "parent", itemId: "parent-later", delta: " still later", hasCustomName: false });
    });
    expect(result.current.state.contextInheritanceByThread.child?.inheritedItems).toEqual([
      expect.objectContaining({ text: "Parent at fork" }),
    ]);
  });

  it("clears old provenance on a successful snapshot without explicit origin", async () => {
    const { result } = renderHook(() => useHarness({ contextInheritanceByThread: { child: {
      origin: { mode: "fresh", parentThreadId: "parent", parentEntryId: null, snapshotEntryId: null }, inheritedItems: null,
    } } }));
    vi.mocked(readThread).mockResolvedValue(snapshot());
    await act(async () => { await result.current.loadThread("workspace", "child"); });
    expect(result.current.state.contextInheritanceByThread.child).toBeNull();
  });

  it("does not overwrite provenance changed by a batched authoritative event before React renders", async () => {
    const pending = deferred<ReturnType<typeof snapshot>>();
    vi.mocked(readThread).mockReturnValueOnce(pending.promise);
    const { result } = renderHook(() => useHarness());
    const request = result.current.loadThread("workspace", "child");
    const newer = { origin: { mode: "fresh" as const, parentThreadId: "new-parent", parentEntryId: null, snapshotEntryId: null }, inheritedItems: null };
    await act(async () => {
      result.current.dispatch({ type: "setThreadContextInheritance", threadId: "child", contextInheritance: newer });
      pending.resolve(snapshot([], { contextOrigin: { mode: "fork", parentThreadId: "old-parent", parentEntryId: null, snapshotEntryId: "seed" } }));
      await request;
    });
    expect(result.current.state.contextInheritanceByThread.child).toBe(newer);
  });

  it("reads into shared state without selecting, creating or resuming a child", async () => {
    vi.mocked(readThread).mockResolvedValue(snapshot([
      { id: "answer", type: "agentMessage", text: "Child answer" },
      { id: "thinking", type: "reasoning", content: ["Child reasoning"] },
    ], { status: { type: "active" }, activeTurnId: "child-turn" }));
    const { result } = renderHook(() => useHarness());

    await act(async () => { await result.current.loadThread("workspace", "child"); });

    expect(readThread).toHaveBeenCalledExactlyOnceWith("workspace", "child");
    expect(result.current.state.activeThreadIdByWorkspace).toEqual({ workspace: "parent" });
    expect(result.current.state.threadsByWorkspace).toEqual({});
    expect(result.current.state.itemsByThread.child).toEqual([
      expect.objectContaining({ id: "answer", text: "Child answer" }),
      expect.objectContaining({ id: "thinking", content: "Child reasoning" }),
    ]);
    expect(result.current.state.threadStatusById.child.isProcessing).toBe(true);
    expect(result.current.state.activeTurnIdByThread.child).toBe("child-turn");
    expect(result.current.state.tokenUsageByThread.child.totalTokens).toBe(100);
  });

  it("deduplicates the same workspace/thread in-flight promise and reads again after settlement", async () => {
    const pending = deferred<ReturnType<typeof snapshot>>();
    vi.mocked(readThread).mockReturnValueOnce(pending.promise);
    const { result } = renderHook(() => useHarness());
    const first = result.current.loadThread("workspace", "child");
    const duplicate = result.current.loadThread("workspace", "child");
    expect(duplicate).toBe(first);
    expect(readThread).toHaveBeenCalledTimes(1);

    await act(async () => { pending.resolve(snapshot()); await first; });
    vi.mocked(readThread).mockResolvedValueOnce(snapshot([{ id: "answer", type: "agentMessage", text: "Fresh" }]));
    await act(async () => { await result.current.loadThread("workspace", "child"); });
    expect(readThread).toHaveBeenCalledTimes(2);
    expect(result.current.state.itemsByThread.child).toEqual([expect.objectContaining({ text: "Fresh" })]);
  });

  it("does not deduplicate reads from different workspaces", async () => {
    const pending = deferred<ReturnType<typeof snapshot>>();
    vi.mocked(readThread).mockReturnValue(pending.promise);
    const { result } = renderHook(() => useHarness());
    const first = result.current.loadThread("workspace", "child");
    const second = result.current.loadThread("other", "child");
    expect(second).not.toBe(first);
    expect(readThread).toHaveBeenCalledTimes(2);
    await act(async () => { pending.resolve(snapshot()); await Promise.all([first, second]); });
  });

  it("keeps batched live text, reasoning, tool completion and status changes while a read is pending", async () => {
    const items: ConversationItem[] = [
      { id: "answer", kind: "message", role: "assistant", text: "Hello" },
      { id: "thinking", kind: "reasoning", summary: "", content: "Consider" },
      { id: "tool", kind: "tool", toolType: "mcpToolCall", title: "Tool: pi / bash", detail: "", output: "running", status: "inProgress" },
    ];
    const pending = deferred<ReturnType<typeof snapshot>>();
    vi.mocked(readThread).mockReturnValue(pending.promise);
    const { result } = renderHook(() => useHarness({ itemsByThread: { child: items } }));
    const request = result.current.loadThread("workspace", "child");

    await act(async () => {
      result.current.dispatch({ type: "appendAgentDelta", workspaceId: "workspace", threadId: "child", itemId: "answer", delta: " world", hasCustomName: false });
      result.current.dispatch({ type: "appendReasoningContent", threadId: "child", itemId: "thinking", delta: " carefully" });
      result.current.dispatch({ type: "upsertItem", workspaceId: "workspace", threadId: "child", item: { ...items[2], status: "completed", output: "done" } as ConversationItem });
      result.current.dispatch({ type: "appendAgentDelta", workspaceId: "workspace", threadId: "child", itemId: "next", delta: "New message", hasCustomName: false });
      result.current.markProcessing("child", false);
      result.current.setActiveTurnId("child", null);
      result.current.dispatch({ type: "setThreadTokenUsage", threadId: "child", tokenUsage: { totalTokens: 300, contextTokens: 80, modelContextWindow: 2000 } });
      pending.resolve(snapshot([
        { id: "user", type: "userMessage", content: [{ type: "text", text: "Original task" }] },
        { id: "answer", type: "agentMessage", text: "Hello" },
        { id: "thinking", type: "reasoning", content: ["Consider"] },
        { id: "tool", type: "mcpToolCall", server: "pi", tool: "bash", status: "inProgress", result: "running" },
      ], { status: { type: "active" }, activeTurnId: "stale-turn" }));
      await request;
    });

    const loaded = result.current.state.itemsByThread.child;
    expect(loaded.map((item) => item.id)).toEqual(["user", "answer", "thinking", "tool", "next"]);
    expect(loaded[1]).toMatchObject({ text: "Hello world" });
    expect(loaded[2]).toMatchObject({ content: "Consider carefully" });
    expect(loaded[3]).toMatchObject({ status: "completed", output: "done" });
    expect(loaded[4]).toMatchObject({ text: "New message" });
    expect(result.current.state.threadStatusById.child.isProcessing).toBe(false);
    expect(result.current.state.activeTurnIdByThread.child).toBeNull();
    expect(result.current.state.tokenUsageByThread.child.totalTokens).toBe(300);
    expect(result.current.state.activeThreadIdByWorkspace.workspace).toBe("parent");
  });

  it("does not repeat snapshot text when queued cumulative prefixes and a newer cumulative update arrive", async () => {
    vi.mocked(readThread).mockResolvedValue(snapshot([
      { id: "agent-1-0", type: "agentMessage", text: "Hello world" },
      { id: "reasoning-1-1", type: "reasoning", content: ["Consider carefully"] },
    ]));
    const { result } = renderHook(() => useHarness());
    await act(async () => {
      await result.current.loadThread("workspace", "child");
      for (const delta of ["Hello", "Hello world", "Hello world again"]) {
        result.current.dispatch({ type: "appendAgentDelta", workspaceId: "workspace", threadId: "child", itemId: "agent-1-0", delta, hasCustomName: false });
      }
      for (const delta of ["Consider", "Consider carefully", "Consider carefully now"]) {
        result.current.dispatch({ type: "appendReasoningContent", threadId: "child", itemId: "reasoning-1-1", delta });
      }
    });
    expect(result.current.state.itemsByThread.child).toEqual([
      expect.objectContaining({ id: "agent-1-0", text: "Hello world again" }),
      expect.objectContaining({ id: "reasoning-1-1", content: "Consider carefully now" }),
    ]);
  });

  it("accepts snapshot progress for untouched items even when normalization clones their references", async () => {
    const pending = deferred<ReturnType<typeof snapshot>>();
    vi.mocked(readThread).mockReturnValue(pending.promise);
    const { result } = renderHook(() => useHarness({ itemsByThread: { child: [
      { id: "answer", kind: "message", role: "assistant", text: "Hello" },
      { id: "tool", kind: "tool", toolType: "mcpToolCall", title: "Tool: pi / bash", detail: "", status: "inProgress" },
    ] } }));
    const request = result.current.loadThread("workspace", "child");
    await act(async () => {
      result.current.dispatch({ type: "appendAgentDelta", workspaceId: "workspace", threadId: "child", itemId: "answer", delta: " world", hasCustomName: false });
      pending.resolve(snapshot([
        { id: "answer", type: "agentMessage", text: "Hello" },
        { id: "tool", type: "mcpToolCall", server: "pi", tool: "bash", status: "completed", result: "done" },
      ]));
      await request;
    });
    expect(result.current.state.itemsByThread.child).toEqual([
      expect.objectContaining({ text: "Hello world" }),
      expect.objectContaining({ status: "completed", output: "done" }),
    ]);
  });

  it("guards status and active turn at reducer time when callers dispatch without the status hook", async () => {
    const pending = deferred<ReturnType<typeof snapshot>>();
    vi.mocked(readThread).mockReturnValue(pending.promise);
    const { result } = renderHook(() => useHarness());
    const request = result.current.loadThread("workspace", "child");
    await act(async () => {
      result.current.dispatch({ type: "markProcessing", threadId: "child", isProcessing: true, timestamp: 1234 });
      result.current.dispatch({ type: "setActiveTurnId", threadId: "child", turnId: "new-turn" });
      pending.resolve(snapshot());
      await request;
    });
    expect(result.current.state.threadStatusById.child.isProcessing).toBe(true);
    expect(result.current.state.activeTurnIdByThread.child).toBe("new-turn");
  });

  it("discards a pending snapshot after context replacement without cancelling a newer read", async () => {
    const old = deferred<ReturnType<typeof snapshot>>();
    const fresh = deferred<ReturnType<typeof snapshot>>();
    vi.mocked(readThread).mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    const { result } = renderHook(() => useHarness());
    const first = result.current.loadThread("workspace", "child");
    act(() => {
      result.current.invalidateThread("child");
      result.current.dispatch({
        type: "replaceThread", workspaceId: "workspace", previousThreadId: "child", threadId: "child",
        items: [{ id: "replacement", kind: "message", role: "assistant", text: "New context" }],
        commands: [], isProcessing: false, turnId: null, timestamp: 1234,
      });
    });
    const second = result.current.loadThread("workspace", "child");
    expect(second).not.toBe(first);
    await act(async () => {
      old.resolve(snapshot([{ id: "stale", type: "agentMessage", text: "Old context" }]));
      await first;
    });
    expect(result.current.loadThread("workspace", "child")).toBe(second);
    expect(result.current.state.itemsByThread.child).toEqual([expect.objectContaining({ text: "New context" })]);
    await act(async () => {
      fresh.resolve(snapshot([{ id: "replacement", type: "agentMessage", text: "New context continued" }]));
      await second;
    });
    expect(result.current.state.itemsByThread.child).toEqual([expect.objectContaining({ text: "New context continued" })]);
  });

  it("preserves a usage-only update even before React renders and never lowers total tokens", async () => {
    const pending = deferred<ReturnType<typeof snapshot>>();
    vi.mocked(readThread).mockReturnValueOnce(pending.promise);
    const { result } = renderHook(() => useHarness());
    const request = result.current.loadThread("workspace", "child");
    await act(async () => {
      result.current.dispatch({ type: "setThreadTokenUsage", threadId: "child", tokenUsage: { totalTokens: 400, contextTokens: 70, modelContextWindow: 3000 } });
      pending.resolve(snapshot());
      await request;
    });
    expect(result.current.state.tokenUsageByThread.child).toEqual({ totalTokens: 400, contextTokens: 70, modelContextWindow: 3000 });

    vi.mocked(readThread).mockResolvedValue(snapshot());
    await act(async () => { await result.current.loadThread("workspace", "child"); });
    expect(result.current.state.tokenUsageByThread.child.totalTokens).toBe(400);
  });

  it("propagates read errors and allows a retry with the same identities", async () => {
    vi.mocked(readThread).mockRejectedValueOnce(new Error("Session unavailable"));
    const { result } = renderHook(() => useHarness());
    await expect(result.current.loadThread("workspace", "child")).rejects.toThrow("Session unavailable");
    expect(result.current.state.itemsByThread.child).toBeUndefined();
    vi.mocked(readThread).mockResolvedValueOnce(snapshot());
    await act(async () => { await result.current.loadThread("workspace", "child"); });
    expect(readThread).toHaveBeenCalledTimes(2);
    expect(result.current.state.itemsByThread.child).toEqual([]);
  });

  it.each([null, {}, { thread: {} }, { thread: [] }, { thread: { id: "wrong", turns: [] } }])(
    "rejects a missing or mismatched thread snapshot: %j",
    async (response) => {
      vi.mocked(readThread).mockResolvedValue(response);
      const { result } = renderHook(() => useHarness());
      await expect(result.current.loadThread("workspace", "child")).rejects.toThrow(/snapshot/i);
      expect(result.current.state.itemsByThread).toEqual({});
    },
  );
});

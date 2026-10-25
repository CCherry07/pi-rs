// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PiEvent, WorkspaceInfo } from "@/types";
import { subscribePiEvents } from "@/services/events";
import { prepareThread, resumeThread, startThread } from "@services/tauri";
import { useThreads } from "./useThreads";

vi.mock("@/services/events", () => ({
  subscribePiEvents: vi.fn(() => vi.fn()),
}));
vi.mock("@services/tauri", () => ({
  prepareThread: vi.fn(),
  reloadThread: vi.fn(),
  startThread: vi.fn(),
  archiveThread: vi.fn(),
  readThread: vi.fn(),
  setThreadName: vi.fn(),
  forkThread: vi.fn(),
  listThreads: vi.fn(),
  listWorkspaces: vi.fn(),
  resumeThread: vi.fn(),
  interruptTurn: vi.fn(),
  sendUserMessage: vi.fn(),
  compactThread: vi.fn(),
}));

const workspace: WorkspaceInfo = {
  id: "workspace",
  name: "Workspace",
  path: "/project",
  settings: { sidebarCollapsed: false },
};
const child = {
  id: "child",
  status: { type: "idle" },
  activeTurnId: null,
  turns: [
    {
      id: "child-turn",
      items: [{ id: "answer", type: "agentMessage", text: "Child history" }],
    },
  ],
};

function emit(method: string, params: Record<string, unknown>) {
  const listener = vi.mocked(subscribePiEvents).mock.calls.slice(-1)[0][0];
  listener({
    workspace_id: workspace.id,
    message: { method, params },
  } as PiEvent);
}

beforeEach(() => {
  vi.resetAllMocks();
  localStorage.clear();
  vi.mocked(subscribePiEvents).mockReturnValue(vi.fn());
  vi.mocked(prepareThread).mockResolvedValue({
    thread: { id: "draft", commands: [] },
  });
  vi.mocked(startThread).mockResolvedValue({
    thread: { id: "parent", commands: [], turns: [] },
  });
  vi.mocked(resumeThread).mockResolvedValue({ thread: child });
});
afterEach(cleanup);

describe("useThreads selected conversation", () => {
  const contextOrigin = {
    mode: "fork",
    parentThreadId: "parent",
    parentEntryId: "cutoff",
    snapshotEntryId: "seed",
  };
  const inheritedContext = {
    turns: [
      {
        items: [
          {
            id: "parent-before",
            type: "agentMessage",
            text: "Frozen parent answer",
          },
        ],
      },
    ],
  };

  it("keeps background child provenance separate and exposes it when the child is selected", async () => {
    const { result } = renderHook(() =>
      useThreads({ activeWorkspace: workspace }),
    );
    await act(async () => {
      await result.current.startThreadForWorkspace(workspace.id);
    });
    const parentItems = result.current.activeItems;
    act(() => {
      emit("thread/started", {
        thread: { ...child, contextOrigin, inheritedContext },
      });
      emit("thread/replaced", {
        previousThreadId: "child",
        thread: { ...child, contextOrigin, inheritedContext },
      });
    });
    expect(result.current.activeThreadId).toBe("parent");
    expect(result.current.activeContextInheritance).toBeNull();
    expect(result.current.activeItems).toBe(parentItems);

    act(() => {
      result.current.setActiveThreadId("child");
    });
    expect(result.current.activeContextInheritance).toEqual({
      origin: contextOrigin,
      inheritedItems: [
        expect.objectContaining({ text: "Frozen parent answer" }),
      ],
    });
    expect(result.current.activeItems).toEqual([
      expect.objectContaining({ text: "Child history" }),
    ]);
    expect(resumeThread).not.toHaveBeenCalled();

    act(() => {
      emit("thread/replaced", {
        previousThreadId: "child",
        thread: { ...child, turns: [] },
      });
    });
    expect(result.current.activeContextInheritance).toBeNull();
    expect(result.current.activeItems).toEqual([]);
  });

  it("hydrates and clears provenance through selection and explicit resume", async () => {
    vi.mocked(resumeThread).mockResolvedValueOnce({
      thread: { ...child, contextOrigin, inheritedContext },
    });
    const { result } = renderHook(() =>
      useThreads({ activeWorkspace: workspace }),
    );
    await act(async () => {
      result.current.setActiveThreadId("child");
    });
    expect(result.current.activeContextInheritance?.origin).toEqual(
      contextOrigin,
    );
    expect(result.current.activeItems).toEqual([
      expect.objectContaining({ text: "Child history" }),
    ]);
    expect(resumeThread).toHaveBeenCalledExactlyOnceWith("workspace", "child");

    vi.mocked(resumeThread).mockResolvedValueOnce({
      thread: { ...child, turns: [] },
    });
    await act(async () => {
      await result.current.refreshThread("workspace", "child");
    });
    expect(result.current.activeContextInheritance).toBeNull();
    expect(result.current.activeItems).toEqual([]);
  });

  it("discards a pending resume after an authoritative same-ID replacement", async () => {
    let resolve!: (response: Record<string, unknown>) => void;
    vi.mocked(resumeThread).mockReturnValueOnce(
      new Promise((complete) => {
        resolve = complete;
      }),
    );
    const { result } = renderHook(() =>
      useThreads({ activeWorkspace: workspace }),
    );
    act(() => {
      result.current.setActiveThreadId("child");
    });
    expect(result.current.threadResumeLoadingById.child).toBe(true);
    await act(async () => {
      emit("thread/replaced", {
        previousThreadId: "child",
        thread: {
          ...child,
          turns: [
            {
              items: [
                { id: "after", type: "agentMessage", text: "After compaction" },
              ],
            },
          ],
        },
      });
      resolve({ thread: { ...child, contextOrigin, inheritedContext } });
    });
    expect(result.current.activeContextInheritance).toBeNull();
    expect(result.current.activeItems).toEqual([
      expect.objectContaining({ id: "after", text: "After compaction" }),
    ]);
    expect(result.current.threadResumeLoadingById.child).toBe(false);
  });

  it("keeps background streaming off the active conversation and shows it on selection", async () => {
    const { result } = renderHook(() =>
      useThreads({ activeWorkspace: workspace }),
    );
    await act(async () => {
      await result.current.startThreadForWorkspace(workspace.id);
    });
    const parentItems = result.current.activeItems;
    act(() => {
      emit("thread/replaced", { previousThreadId: "child", thread: child });
      emit("turn/started", {
        threadId: "child",
        turn: { id: "followup", status: "inProgress" },
      });
      emit("item/agentMessage/delta", {
        threadId: "child",
        itemId: "followup-answer",
        delta: "Live reply",
      });
      emit("thread/tokenUsage/updated", {
        threadId: "child",
        tokenUsage: { totalTokens: 450 },
      });
    });
    expect(result.current.activeThreadId).toBe("parent");
    expect(result.current.activeItems).toBe(parentItems);
    expect(result.current.threadStatusById.child.isProcessing).toBe(true);
    expect(result.current.tokenUsageByThread.child.totalTokens).toBe(450);
    act(() => {
      result.current.setActiveThreadId("child");
    });
    expect(result.current.activeItems).toEqual([
      expect.objectContaining({ text: "Child history" }),
      expect.objectContaining({ text: "Live reply" }),
    ]);
  });

  it("hydrates replacement usage in event order and lets later live usage win", async () => {
    const { result } = renderHook(() =>
      useThreads({ activeWorkspace: workspace }),
    );
    await act(async () => {
      await result.current.startThreadForWorkspace(workspace.id);
    });
    act(() => {
      emit("thread/tokenUsage/updated", {
        threadId: "child",
        tokenUsage: { totalTokens: 100, contextTokens: 90 },
      });
      emit("thread/replaced", {
        previousThreadId: "child",
        thread: {
          ...child,
          tokenUsage: {
            total_tokens: "300",
            context_tokens: "20",
            model_context_window: "2000",
          },
        },
      });
    });
    expect(result.current.tokenUsageByThread.child).toEqual({
      totalTokens: 300,
      contextTokens: 20,
      modelContextWindow: 2000,
    });
    act(() => {
      emit("thread/replaced", {
        previousThreadId: "child",
        thread: { ...child, tokenUsage: { totalTokens: 400 } },
      });
      emit("thread/tokenUsage/updated", {
        threadId: "child",
        tokenUsage: { totalTokens: 500, contextTokens: 40 },
      });
    });
    expect(result.current.tokenUsageByThread.child).toMatchObject({
      totalTokens: 500,
      contextTokens: 40,
    });
    expect(result.current.activeThreadId).toBe("parent");
  });
});

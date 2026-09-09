// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "@/types";
import { initialState } from "./useThreadsReducer";
import { useThreadActions } from "./useThreadActions";

const { forkThread, listThreads, listWorkspaces, resumeThread } = vi.hoisted(() => ({
  forkThread: vi.fn(),
  listThreads: vi.fn(),
  listWorkspaces: vi.fn(),
  resumeThread: vi.fn(),
}));

vi.mock("@services/tauri", () => ({
  archiveThread: vi.fn(),
  forkThread,
  listThreads,
  listWorkspaces,
  resumeThread,
  startThread: vi.fn(),
}));

function workspace(id: string, path: string): WorkspaceInfo {
  return { id, name: id, path, settings: { sidebarCollapsed: false } };
}

function setup(
  threadsByWorkspace = initialState.threadsByWorkspace,
) {
  const dispatch = vi.fn();
  const updateThreadParent = vi.fn();
  const renameThread = vi.fn();
  const { result } = renderHook(() =>
    useThreadActions({
      dispatch,
      itemsByThread: initialState.itemsByThread,
      threadsByWorkspace,
      activeThreadIdByWorkspace: initialState.activeThreadIdByWorkspace,
      activeTurnIdByThread: initialState.activeTurnIdByThread,
      threadParentById: initialState.threadParentById,
      threadListCursorByWorkspace: initialState.threadListCursorByWorkspace,
      threadStatusById: initialState.threadStatusById,
      threadSortKey: "updated_at",
      getCustomName: () => undefined,
      renameThread,
      threadActivityRef: { current: {} },
      loadedThreadsRef: { current: {} },
      replaceOnResumeRef: { current: {} },
      applyCollabThreadLinksFromThread: vi.fn(),
      updateThreadParent,
      onSubagentThreadDetected: vi.fn(),
    }),
  );
  return { dispatch, renameThread, result, updateThreadParent };
}

describe("useThreadActions", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listThreads.mockResolvedValue({ data: [], nextCursor: null });
    listWorkspaces.mockResolvedValue([]);
    resumeThread.mockResolvedValue({ thread: { id: "forked-thread", turns: [] } });
  });

  it("loads the session menu for every requested workspace", async () => {
    const first = workspace("workspace-1", "/projects/one");
    const second = workspace("workspace-2", "/projects/two");
    listThreads.mockImplementation(async (workspaceId: string) => ({
      data: [
        {
          id: `thread-for-${workspaceId}`,
          cwd: workspaceId === first.id ? first.path : second.path,
          preview: `Session for ${workspaceId}`,
          updatedAt: 1,
        },
      ],
      nextCursor: null,
    }));
    const { dispatch, result } = setup();

    await act(async () => {
      await result.current.listThreadsForWorkspaces([first, second]);
    });

    expect(listThreads).toHaveBeenCalledWith(
      first.id,
      null,
      expect.any(Number),
      "updated_at",
    );
    expect(listThreads).toHaveBeenCalledWith(
      second.id,
      null,
      expect.any(Number),
      "updated_at",
    );
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "setThreads",
        workspaceId: second.id,
        threads: [
          expect.objectContaining({
            id: `thread-for-${second.id}`,
            name: `Session for ${second.id}`,
          }),
        ],
      }),
    );
  });

  it("forks at the selected message entry and links the new thread", async () => {
    forkThread.mockResolvedValue({ thread: { id: "forked-thread" } });
    const { renameThread, result, updateThreadParent } = setup({
      "workspace-1": [
        { id: "source-thread", name: "Investigate auth", updatedAt: 3 },
        { id: "older-fork", name: "Investigate auth (1)", updatedAt: 2 },
      ],
    });

    await act(async () => {
      await expect(
        result.current.forkThreadForWorkspace(
          "workspace-1",
          "source-thread",
          "message-entry-1",
        ),
      ).resolves.toBe("forked-thread");
    });

    expect(forkThread).toHaveBeenCalledWith(
      "workspace-1",
      "source-thread",
      "message-entry-1",
    );
    expect(updateThreadParent).toHaveBeenCalledWith("source-thread", [
      "forked-thread",
    ]);
    expect(renameThread).toHaveBeenCalledWith(
      "workspace-1",
      "forked-thread",
      "Investigate auth (2)",
    );
  });
});

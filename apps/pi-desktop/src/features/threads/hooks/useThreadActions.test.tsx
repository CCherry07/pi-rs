// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "@/types";
import { initialState } from "./useThreadsReducer";
import { useThreadActions } from "./useThreadActions";

const { listThreads, listWorkspaces } = vi.hoisted(() => ({
  listThreads: vi.fn(),
  listWorkspaces: vi.fn(),
}));

vi.mock("@services/tauri", () => ({
  archiveThread: vi.fn(),
  forkThread: vi.fn(),
  listThreads,
  listWorkspaces,
  resumeThread: vi.fn(),
  startThread: vi.fn(),
}));

function workspace(id: string, path: string): WorkspaceInfo {
  return { id, name: id, path, settings: { sidebarCollapsed: false } };
}

function setup() {
  const dispatch = vi.fn();
  const { result } = renderHook(() =>
    useThreadActions({
      dispatch,
      itemsByThread: initialState.itemsByThread,
      threadsByWorkspace: initialState.threadsByWorkspace,
      activeThreadIdByWorkspace: initialState.activeThreadIdByWorkspace,
      activeTurnIdByThread: initialState.activeTurnIdByThread,
      threadParentById: initialState.threadParentById,
      threadListCursorByWorkspace: initialState.threadListCursorByWorkspace,
      threadStatusById: initialState.threadStatusById,
      threadSortKey: "updated_at",
      getCustomName: () => undefined,
      threadActivityRef: { current: {} },
      loadedThreadsRef: { current: {} },
      replaceOnResumeRef: { current: {} },
      applyCollabThreadLinksFromThread: vi.fn(),
      updateThreadParent: vi.fn(),
      onSubagentThreadDetected: vi.fn(),
    }),
  );
  return { dispatch, result };
}

describe("useThreadActions", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listThreads.mockResolvedValue({ data: [], nextCursor: null });
    listWorkspaces.mockResolvedValue([]);
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
});

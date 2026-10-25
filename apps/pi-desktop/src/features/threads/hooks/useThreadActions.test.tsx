// @vitest-environment jsdom
import { useCallback, useReducer, useRef } from "react";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "@/types";
import type { ThreadListPage, ThreadListRequest } from "@services/tauri";
import {
  initialState,
  threadReducer,
  type ThreadAction,
  type ThreadState,
} from "./useThreadsReducer";
import { useThreadActions } from "./useThreadActions";

const { forkThread, listThreads, resumeThread } = vi.hoisted(() => ({
  forkThread: vi.fn(),
  listThreads: vi.fn(),
  resumeThread: vi.fn(),
}));
vi.mock("@services/tauri", () => ({
  archiveThread: vi.fn(),
  forkThread,
  listThreads,
  resumeThread,
  startThread: vi.fn(),
}));

function workspace(id: string, path = "/projects/current"): WorkspaceInfo {
  return { id, name: id, path, settings: { sidebarCollapsed: false } };
}
function page(
  id: string,
  ids: string[],
  nextCursor: string | null = null,
): ThreadListPage {
  return {
    workspaceId: id,
    data: ids.map((id) => ({
      id,
      preview: id,
      cwd: "/projects/old",
      updatedAt: 1,
    })),
    nextCursor,
  };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}
function setup(seed: Partial<ThreadState> = {}) {
  const dispatchSpy = vi.fn();
  const updateThreadParent = vi.fn();
  const renameThread = vi.fn();
  const { result } = renderHook(() => {
    const [state, reduce] = useReducer(threadReducer, {
      ...initialState,
      ...seed,
    });
    const dispatch = useCallback((action: ThreadAction) => {
      dispatchSpy(action);
      reduce(action);
    }, []);
    const actions = useThreadActions({
      ...state,
      dispatch,
      threadSortKey: "updated_at",
      getCustomName: () => undefined,
      renameThread,
      threadActivityRef: useRef({}),
      loadedThreadsRef: useRef({}),
      replaceOnResumeRef: useRef({}),
      applyCollabThreadLinksFromThread: vi.fn(),
      updateThreadParent,
      onSubagentThreadDetected: vi.fn(),
    });
    return { ...actions, state, dispatch };
  });
  return { dispatch: dispatchSpy, renameThread, result, updateThreadParent };
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  listThreads.mockImplementation(async (requests: ThreadListRequest[]) =>
    requests.map(({ workspaceId }) => page(workspaceId, [])),
  );
  resumeThread.mockResolvedValue({
    thread: { id: "forked-thread", turns: [] },
  });
});
afterEach(cleanup);

describe("useThreadActions", () => {
  it("batches project-scoped listings and preserves sessions saved before a directory change", async () => {
    const first = workspace("one");
    const second = workspace("two", "/another/root");
    listThreads.mockResolvedValue([
      page("one", ["old-cwd-session"]),
      page("two", ["second-session"]),
    ]);
    const { result } = setup();
    await act(async () => {
      await result.current.listThreadsForWorkspaces([first, second, first]);
    });
    expect(listThreads).toHaveBeenCalledExactlyOnceWith(
      [{ workspaceId: "one" }, { workspaceId: "two" }],
      { limit: 20, sortKey: "updated_at" },
    );
    expect(
      result.current.state.threadsByWorkspace.one.map((thread) => thread.id),
    ).toEqual(["old-cwd-session"]);
    expect(
      result.current.state.threadsByWorkspace.two.map((thread) => thread.id),
    ).toEqual(["second-session"]);
  });

  it("follows the backend cursor and appends without dropping sessions created during the request", async () => {
    const project = workspace("one");
    const { result } = setup();
    listThreads.mockResolvedValueOnce([
      page("one", ["recent"], "cursor-from-native"),
    ]);
    await act(async () => {
      await result.current.listThreadsForWorkspace(project);
    });
    const older = deferred<ThreadListPage[]>();
    listThreads.mockReturnValueOnce(older.promise);
    let pending!: Promise<void>;
    act(() => {
      pending = result.current.loadOlderThreadsForWorkspace(project);
    });
    expect(listThreads).toHaveBeenLastCalledWith(
      [{ workspaceId: "one", cursor: "cursor-from-native" }],
      { limit: 20, sortKey: "updated_at" },
    );
    act(() => {
      result.current.dispatch({
        type: "ensureThread",
        workspaceId: "one",
        threadId: "just-started",
      });
    });
    await act(async () => {
      older.resolve([page("one", ["recent", "oldest"])]);
      await pending;
    });
    expect(
      result.current.state.threadsByWorkspace.one.map((thread) => thread.id),
    ).toEqual(["just-started", "recent", "oldest"]);
    expect(result.current.state.activeThreadIdByWorkspace.one).toBe(
      "just-started",
    );
    expect(result.current.state.threadListCursorByWorkspace.one).toBeNull();
    expect(result.current.state.threadListPagingByWorkspace.one).toBe(false);
  });

  it("discards an older list response after the user changes sorting", async () => {
    const old = deferred<ThreadListPage[]>();
    listThreads
      .mockReturnValueOnce(old.promise)
      .mockResolvedValueOnce([
        page("one", ["created-newest"], "created-cursor"),
      ]);
    const { result } = setup();
    let pending!: Promise<void>;
    act(() => {
      pending = result.current.listThreadsForWorkspace(workspace("one"));
    });
    await act(async () => {
      await result.current.listThreadsForWorkspace(workspace("one"), {
        sortKey: "created_at",
      });
    });
    await act(async () => {
      old.resolve([page("one", ["outdated"], "old-cursor")]);
      await pending;
    });
    expect(listThreads).toHaveBeenLastCalledWith([{ workspaceId: "one" }], {
      limit: 20,
      sortKey: "created_at",
    });
    expect(
      result.current.state.threadsByWorkspace.one.map((thread) => thread.id),
    ).toEqual(["created-newest"]);
    expect(result.current.state.threadListCursorByWorkspace.one).toBe(
      "created-cursor",
    );
    expect(result.current.state.threadSortKeyByWorkspace.one).toBe(
      "created_at",
    );
  });

  it("discards a pending older page after refreshing the list", async () => {
    const older = deferred<ThreadListPage[]>();
    listThreads
      .mockReturnValueOnce(older.promise)
      .mockResolvedValueOnce([page("one", ["refreshed"], "fresh-cursor")]);
    const { result } = setup({
      threadListCursorByWorkspace: { one: "older-cursor" },
    });
    let pending!: Promise<void>;
    act(() => {
      pending = result.current.loadOlderThreadsForWorkspace(workspace("one"));
    });
    await act(async () => {
      await result.current.listThreadsForWorkspace(workspace("one"));
    });
    await act(async () => {
      older.resolve([page("one", ["stale-page"])]);
      await pending;
    });
    expect(
      result.current.state.threadsByWorkspace.one.map((thread) => thread.id),
    ).toEqual(["refreshed"]);
    expect(result.current.state.threadListCursorByWorkspace.one).toBe(
      "fresh-cursor",
    );
    expect(result.current.state.threadListPagingByWorkspace.one).toBe(false);
  });

  it("keeps the existing list and retry cursor when a page fails", async () => {
    listThreads.mockRejectedValueOnce(new Error("read failed"));
    const { result } = setup({
      threadsByWorkspace: {
        one: [{ id: "recent", name: "Recent", updatedAt: 1 }],
      },
      threadListCursorByWorkspace: { one: "retry-cursor" },
    });
    await act(async () => {
      await result.current.loadOlderThreadsForWorkspace(workspace("one"));
    });
    expect(result.current.state.threadsByWorkspace.one[0].id).toBe("recent");
    expect(result.current.state.threadListCursorByWorkspace.one).toBe(
      "retry-cursor",
    );
    expect(result.current.state.threadListPagingByWorkspace.one).toBe(false);
  });

  it("forks at the selected message entry and links the new thread", async () => {
    forkThread.mockResolvedValue({ thread: { id: "forked-thread" } });
    const { renameThread, result, updateThreadParent } = setup({
      threadsByWorkspace: {
        "workspace-1": [
          { id: "source-thread", name: "Investigate auth", updatedAt: 3 },
          { id: "older-fork", name: "Investigate auth (1)", updatedAt: 2 },
        ],
      },
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

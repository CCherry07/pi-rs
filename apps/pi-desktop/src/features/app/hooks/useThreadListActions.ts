import { useCallback } from "react";
import type { ThreadListSortKey, WorkspaceInfo } from "../../../types";

type ListThreadsOptions = {
  sortKey?: ThreadListSortKey;
};

type UseThreadListActionsOptions = {
  threadListSortKey: ThreadListSortKey;
  setThreadListSortKey: (sortKey: ThreadListSortKey) => void;
  workspaces: WorkspaceInfo[];
  refreshWorkspaces: () => Promise<WorkspaceInfo[] | undefined>;
  listThreadsForWorkspaces: (
    workspaces: WorkspaceInfo[],
    options?: ListThreadsOptions,
  ) => void | Promise<void>;
  resetWorkspaceThreads: (workspaceId: string) => void;
};

export function useThreadListActions({
  threadListSortKey,
  setThreadListSortKey,
  workspaces,
  refreshWorkspaces,
  listThreadsForWorkspaces,
  resetWorkspaceThreads,
}: UseThreadListActionsOptions) {
  const handleSetThreadListSortKey = useCallback(
    (nextSortKey: ThreadListSortKey) => {
      if (nextSortKey === threadListSortKey) {
        return;
      }
      setThreadListSortKey(nextSortKey);
      if (workspaces.length > 0) {
        void listThreadsForWorkspaces(workspaces, { sortKey: nextSortKey });
      }
    },
    [threadListSortKey, setThreadListSortKey, workspaces, listThreadsForWorkspaces],
  );

  const handleRefreshAllWorkspaceThreads = useCallback(async () => {
    const refreshed = await refreshWorkspaces();
    const source = refreshed ?? workspaces;
    source.forEach((workspace) => {
      resetWorkspaceThreads(workspace.id);
    });
    if (source.length > 0) {
      await listThreadsForWorkspaces(source);
    }
  }, [refreshWorkspaces, workspaces, resetWorkspaceThreads, listThreadsForWorkspaces]);

  return {
    handleSetThreadListSortKey,
    handleRefreshAllWorkspaceThreads,
  };
}

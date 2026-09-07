import { useEffect, useRef } from "react";
import type { WorkspaceInfo } from "../../../types";

const INITIAL_THREAD_LIST_MAX_PAGES = 6;

type WorkspaceRestoreOptions = {
  workspaces: WorkspaceInfo[];
  hasLoaded: boolean;
  listThreadsForWorkspaces: (
    workspaces: WorkspaceInfo[],
    options?: { preserveState?: boolean; maxPages?: number },
  ) => Promise<void>;
};

export function useWorkspaceRestore({
  workspaces,
  hasLoaded,
  listThreadsForWorkspaces,
}: WorkspaceRestoreOptions) {
  const restoredWorkspaces = useRef(new Set<string>());

  useEffect(() => {
    if (!hasLoaded) {
      return;
    }
    const pending = workspaces.filter(
      (workspace) => !restoredWorkspaces.current.has(workspace.id),
    );
    if (pending.length === 0) {
      return;
    }
    pending.forEach((workspace) => {
      restoredWorkspaces.current.add(workspace.id);
    });
    void listThreadsForWorkspaces(pending, {
      maxPages: INITIAL_THREAD_LIST_MAX_PAGES,
    });
  }, [hasLoaded, listThreadsForWorkspaces, workspaces]);
}

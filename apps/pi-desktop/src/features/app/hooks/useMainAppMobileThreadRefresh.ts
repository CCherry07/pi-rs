import { useCallback, useState } from "react";
import type { WorkspaceInfo } from "@/types";

type UseMainAppMobileThreadRefreshArgs = {
  activeWorkspace: WorkspaceInfo | null;
  activeThreadId: string | null;
  startThreadForWorkspace: (
    workspaceId: string,
    options?: { activate?: boolean },
  ) => Promise<string | null>;
  refreshThread: (workspaceId: string, threadId: string) => Promise<unknown>;
};

export function useMainAppMobileThreadRefresh({
  activeWorkspace,
  activeThreadId,
  startThreadForWorkspace,
  refreshThread,
}: UseMainAppMobileThreadRefreshArgs) {
  const [mobileThreadRefreshLoading, setMobileThreadRefreshLoading] = useState(false);

  const handleMobileThreadRefresh = useCallback(() => {
    if (mobileThreadRefreshLoading || !activeWorkspace) {
      return;
    }
    setMobileThreadRefreshLoading(true);
    void (async () => {
      let threadId = activeThreadId;
      if (!threadId) {
        threadId = await startThreadForWorkspace(activeWorkspace.id, {
          activate: true,
        });
      }
      if (!threadId) {
        return;
      }
      await refreshThread(activeWorkspace.id, threadId);
    })()
      .catch(() => {
        // Existing thread actions surface errors through debug entries and toasts.
      })
      .finally(() => {
        setMobileThreadRefreshLoading(false);
      });
  }, [
    activeThreadId,
    activeWorkspace,
    mobileThreadRefreshLoading,
    refreshThread,
    startThreadForWorkspace,
  ]);

  return {
    mobileThreadRefreshLoading,
    handleMobileThreadRefresh,
  };
}

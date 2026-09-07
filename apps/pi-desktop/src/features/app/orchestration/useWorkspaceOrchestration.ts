import { useCallback, useMemo } from "react";
import type { WorkspaceInfo } from "@/types";

type ThreadSummary = {
  id: string;
  name?: string | null;
  updatedAt: number;
};

type LastAgentMessage = {
  text: string;
  timestamp: number;
};

type ThreadStatus = {
  isProcessing?: boolean;
};

type UseWorkspaceInsightsOrchestrationOptions = {
  workspaces: WorkspaceInfo[];
  hasLoaded: boolean;
  threadsByWorkspace: Record<string, ThreadSummary[]>;
  lastAgentMessageByThread: Record<string, LastAgentMessage | undefined>;
  threadStatusById: Record<string, ThreadStatus | undefined>;
  threadListLoadingByWorkspace: Record<string, boolean | undefined>;
  getWorkspaceGroupName: (workspaceId: string) => string | null | undefined;
};

type UseWorkspaceOrderingOrchestrationOptions = {
  workspaces: WorkspaceInfo[];
  workspacesById: Map<string, WorkspaceInfo>;
  updateWorkspaceSettings: (
    workspaceId: string,
    settings: Partial<WorkspaceInfo["settings"]>,
  ) => Promise<unknown>;
};

export function useWorkspaceInsightsOrchestration({
  workspaces,
  hasLoaded,
  threadsByWorkspace,
  lastAgentMessageByThread,
  threadStatusById,
  threadListLoadingByWorkspace,
  getWorkspaceGroupName,
}: UseWorkspaceInsightsOrchestrationOptions) {
  const latestAgentRuns = useMemo(() => {
    const entries: Array<{
      threadId: string;
      message: string;
      timestamp: number;
      projectName: string;
      groupName?: string | null;
      workspaceId: string;
      isProcessing: boolean;
    }> = [];

    workspaces.forEach((workspace) => {
      const threads = threadsByWorkspace[workspace.id] ?? [];
      threads.forEach((thread) => {
        const entry = lastAgentMessageByThread[thread.id];
        if (!entry) {
          return;
        }
        entries.push({
          threadId: thread.id,
          message: entry.text,
          timestamp: entry.timestamp,
          projectName: workspace.name,
          groupName: getWorkspaceGroupName(workspace.id),
          workspaceId: workspace.id,
          isProcessing: threadStatusById[thread.id]?.isProcessing ?? false,
        });
      });
    });

    return entries.sort((a, b) => b.timestamp - a.timestamp).slice(0, 3);
  }, [
    getWorkspaceGroupName,
    lastAgentMessageByThread,
    threadStatusById,
    threadsByWorkspace,
    workspaces,
  ]);

  const isLoadingLatestAgents = useMemo(
    () =>
      !hasLoaded || workspaces.some((workspace) => threadListLoadingByWorkspace[workspace.id] ?? false),
    [hasLoaded, threadListLoadingByWorkspace, workspaces],
  );

  return {
    latestAgentRuns,
    isLoadingLatestAgents,
  };
}

export function useWorkspaceOrderingOrchestration({
  workspaces,
  workspacesById,
  updateWorkspaceSettings,
}: UseWorkspaceOrderingOrchestrationOptions) {
  const orderValue = useCallback(
    (entry: WorkspaceInfo) =>
      typeof entry.settings.sortOrder === "number"
        ? entry.settings.sortOrder
        : Number.MAX_SAFE_INTEGER,
    [],
  );

  const handleMoveWorkspace = useCallback(
    async (workspaceId: string, direction: "up" | "down") => {
      const target = workspacesById.get(workspaceId);
      if (!target || (target.kind ?? "main") === "worktree") {
        return;
      }

      const targetGroupId = target.settings.groupId ?? null;
      const ordered = workspaces
        .filter(
          (entry) =>
            (entry.kind ?? "main") !== "worktree" &&
            (entry.settings.groupId ?? null) === targetGroupId,
        )
        .slice()
        .sort((a, b) => {
          const orderDiff = orderValue(a) - orderValue(b);
          if (orderDiff !== 0) {
            return orderDiff;
          }
          return a.name.localeCompare(b.name);
        });

      const index = ordered.findIndex((entry) => entry.id === workspaceId);
      if (index === -1) {
        return;
      }

      const nextIndex = direction === "up" ? index - 1 : index + 1;
      if (nextIndex < 0 || nextIndex >= ordered.length) {
        return;
      }

      const next = ordered.slice();
      const temp = next[index];
      next[index] = next[nextIndex];
      next[nextIndex] = temp;

      await Promise.all(
        next.map((entry, idx) =>
          updateWorkspaceSettings(entry.id, {
            sortOrder: idx,
          }),
        ),
      );
    },
    [orderValue, updateWorkspaceSettings, workspaces, workspacesById],
  );

  return { handleMoveWorkspace };
}

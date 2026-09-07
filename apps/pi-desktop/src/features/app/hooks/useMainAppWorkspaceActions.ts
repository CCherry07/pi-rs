import { useCallback } from "react";
import { useWorkspaceDropZone } from "@/features/workspaces/hooks/useWorkspaceDropZone";
import { useWorkspaceActions } from "@app/hooks/useWorkspaceActions";

type UseMainAppWorkspaceActionsArgs = {
  workspaceActions: Parameters<typeof useWorkspaceActions>[0];
};

export function useMainAppWorkspaceActions({
  workspaceActions,
}: UseMainAppWorkspaceActionsArgs) {
  const actionState = useWorkspaceActions(workspaceActions);

  const handleDropWorkspacePaths = useCallback(
    async (paths: string[]) => {
      const uniquePaths = Array.from(new Set(paths.filter((path) => path.length > 0)));
      if (uniquePaths.length === 0) {
        return;
      }
      void actionState.handleAddWorkspacesFromPaths(uniquePaths);
    },
    [actionState],
  );

  const dropZoneState = useWorkspaceDropZone({
    onDropPaths: handleDropWorkspacePaths,
  });

  return {
    ...actionState,
    ...dropZoneState,
  };
}

import { useCallback } from "react";
import { useWorkspaces } from "../../workspaces/hooks/useWorkspaces";
import type { AppSettings, WorkspaceInfo } from "../../../types";
import type { DebugEntry } from "../../../types";
import { useWorkspaceDialogs } from "./useWorkspaceDialogs";
import { useWorkspaceProjectPrompt } from "../../workspaces/hooks/useWorkspaceProjectPrompt";

type WorkspaceControllerOptions = {
  appSettings: AppSettings;
  addDebugEntry: (entry: DebugEntry) => void;
  queueSaveSettings: (next: AppSettings) => Promise<AppSettings>;
};

export function useWorkspaceController({
  appSettings,
  addDebugEntry,
  queueSaveSettings,
}: WorkspaceControllerOptions) {
  const workspaceCore = useWorkspaces({
    onDebug: addDebugEntry,
    appSettings,
    onUpdateAppSettings: queueSaveSettings,
  });

  const {
    workspaces,
    addWorkspacesFromPaths: addWorkspacesFromPathsCore,
    removeWorkspace: removeWorkspaceCore,
    removeWorktree: removeWorktreeCore,
  } = workspaceCore;

  const {
    showAddWorkspacesResult,
    confirmWorkspaceRemoval,
    confirmWorktreeRemoval,
    showWorkspaceRemovalError,
    showWorktreeRemovalError,
  } = useWorkspaceDialogs();

  const runAddWorkspacesFromPaths = useCallback(
    async (paths: string[]) => {
      const result = await addWorkspacesFromPathsCore(paths);
      await showAddWorkspacesResult(result);
      return result;
    },
    [addWorkspacesFromPathsCore, showAddWorkspacesResult],
  );

  const addWorkspacesFromPaths = useCallback(
    async (paths: string[]): Promise<WorkspaceInfo | null> => {
      const result = await runAddWorkspacesFromPaths(paths);
      return result.firstAdded;
    },
    [runAddWorkspacesFromPaths],
  );

  const workspaceProjectPrompt = useWorkspaceProjectPrompt({
    onSubmit: workspaceCore.createWorkspaceProject,
    onUpdate: workspaceCore.updateWorkspaceProject,
  });

  const removeWorkspace = useCallback(
    async (workspaceId: string) => {
      const confirmed = await confirmWorkspaceRemoval(workspaces, workspaceId);
      if (!confirmed) {
        return;
      }
      try {
        await removeWorkspaceCore(workspaceId);
      } catch (error) {
        await showWorkspaceRemovalError(error);
      }
    },
    [confirmWorkspaceRemoval, removeWorkspaceCore, showWorkspaceRemovalError, workspaces],
  );

  const removeWorktree = useCallback(
    async (workspaceId: string) => {
      const confirmed = await confirmWorktreeRemoval(workspaces, workspaceId);
      if (!confirmed) {
        return;
      }
      try {
        await removeWorktreeCore(workspaceId);
      } catch (error) {
        await showWorktreeRemovalError(error, Boolean(workspaces.find((entry) => entry.id === workspaceId)?.worktree?.managed));
      }
    },
    [confirmWorktreeRemoval, removeWorktreeCore, showWorktreeRemovalError, workspaces],
  );

  return {
    ...workspaceCore,
    addWorkspace: workspaceProjectPrompt.request,
    editWorkspace: workspaceProjectPrompt.edit,
    workspaceProjectPrompt,
    addWorkspacesFromPaths,
    removeWorkspace,
    removeWorktree,
  };
}

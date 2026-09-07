import { useEffect, useRef } from "react";
import type { WorkspaceInfo } from "@/types";
import type { WorktreeRenameState } from "@/features/layout/hooks/layoutNodes/types";

type RenameWorktreePromptState = {
  workspaceId: string;
  name: string;
  originalName: string;
  error: string | null;
  isSubmitting: boolean;
};

type RenameWorktreeUpstreamPromptState = {
  workspaceId: string;
  oldBranch: string;
  newBranch: string;
  error: string | null;
  isSubmitting: boolean;
};

type UseMainAppWorktreeStateArgs = {
  activeWorkspace: WorkspaceInfo | null;
  workspacesById: Map<string, WorkspaceInfo>;
  renameWorktreePrompt: RenameWorktreePromptState | null;
  renameWorktreeNotice: string | null;
  renameWorktreeUpstreamPrompt: RenameWorktreeUpstreamPromptState | null;
  confirmRenameWorktreeUpstream: () => void;
  handleOpenRenameWorktree: () => void;
  handleRenameWorktreeChange: (value: string) => void;
  handleRenameWorktreeCancel: () => void;
  handleRenameWorktreeConfirm: () => void;
};

export function useMainAppWorktreeState({
  activeWorkspace,
  workspacesById,
  renameWorktreePrompt,
  renameWorktreeNotice,
  renameWorktreeUpstreamPrompt,
  confirmRenameWorktreeUpstream,
  handleOpenRenameWorktree,
  handleRenameWorktreeChange,
  handleRenameWorktreeCancel,
  handleRenameWorktreeConfirm,
}: UseMainAppWorktreeStateArgs) {
  const isWorktreeWorkspace = activeWorkspace?.kind === "worktree";
  const activeParentWorkspace = isWorktreeWorkspace
    ? workspacesById.get(activeWorkspace?.parentId ?? "") ?? null
    : null;
  const worktreeLabel = isWorktreeWorkspace
    ? (activeWorkspace?.name?.trim() || activeWorkspace?.worktree?.branch) ?? null
    : null;
  const activeRenamePrompt =
    renameWorktreePrompt?.workspaceId === activeWorkspace?.id ? renameWorktreePrompt : null;
  const worktreeRename: WorktreeRenameState | null =
    isWorktreeWorkspace && activeWorkspace
      ? {
          name: activeRenamePrompt?.name ?? worktreeLabel ?? "",
          error: activeRenamePrompt?.error ?? null,
          notice: renameWorktreeNotice,
          isSubmitting: activeRenamePrompt?.isSubmitting ?? false,
          isDirty: activeRenamePrompt
            ? activeRenamePrompt.name.trim() !== activeRenamePrompt.originalName.trim()
            : false,
          upstream:
            renameWorktreeUpstreamPrompt?.workspaceId === activeWorkspace.id
              ? {
                  oldBranch: renameWorktreeUpstreamPrompt.oldBranch,
                  newBranch: renameWorktreeUpstreamPrompt.newBranch,
                  error: renameWorktreeUpstreamPrompt.error,
                  isSubmitting: renameWorktreeUpstreamPrompt.isSubmitting,
                  onConfirm: confirmRenameWorktreeUpstream,
                }
              : null,
          onFocus: handleOpenRenameWorktree,
          onChange: handleRenameWorktreeChange,
          onCancel: handleRenameWorktreeCancel,
          onCommit: handleRenameWorktreeConfirm,
        }
      : null;

  const baseWorkspaceRef = useRef(activeParentWorkspace ?? activeWorkspace);

  useEffect(() => {
    baseWorkspaceRef.current = activeParentWorkspace ?? activeWorkspace;
  }, [activeParentWorkspace, activeWorkspace]);

  return {
    isWorktreeWorkspace,
    activeParentWorkspace,
    worktreeLabel,
    worktreeRename,
    baseWorkspaceRef,
  };
}

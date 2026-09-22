import { useCallback, type MouseEvent } from "react";
import { Menu, MenuItem } from "@tauri-apps/api/menu";
import { LogicalPosition } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";

import type { WorkspaceInfo } from "../../../types";
import type { WorkspaceHoverAction } from "../components/WorkspaceHoverContents";
import { pushErrorToast } from "../../../services/toasts";
import { fileManagerName } from "../../../utils/platformPaths";

type SidebarMenuHandlers = {
  onAddAgent: (workspace: WorkspaceInfo) => void;
  onAddWorktreeAgent: (workspace: WorkspaceInfo) => void;
  onAddCloneAgent: (workspace: WorkspaceInfo) => void;
  onDeleteThread: (workspaceId: string, threadId: string) => void;
  onSyncThread: (workspaceId: string, threadId: string) => void;
  onPinThread: (workspaceId: string, threadId: string) => void;
  onUnpinThread: (workspaceId: string, threadId: string) => void;
  isThreadPinned: (workspaceId: string, threadId: string) => boolean;
  onRenameThread: (workspaceId: string, threadId: string) => void;
  onReloadWorkspaceThreads: (workspaceId: string) => void;
  onEditWorkspace: (workspaceId: string) => void;
  onDeleteWorkspace: (workspaceId: string) => void;
  onDeleteWorktree: (workspaceId: string) => void;
};

async function showWorkspaceActionsMenu(
  event: MouseEvent,
  actions: WorkspaceHoverAction[],
) {
  event.preventDefault();
  event.stopPropagation();
  const items = await Promise.all(
    actions.map((action) => MenuItem.new({
      text: action.label,
      action: action.onSelect,
    })),
  );
  const menu = await Menu.new({ items });
  const window = getCurrentWindow();
  const position = new LogicalPosition(event.clientX, event.clientY);
  await menu.popup(position, window);
}

export function useSidebarMenus({
  onAddAgent,
  onAddWorktreeAgent,
  onAddCloneAgent,
  onDeleteThread,
  onSyncThread,
  onPinThread,
  onUnpinThread,
  isThreadPinned,
  onRenameThread,
  onReloadWorkspaceThreads,
  onEditWorkspace,
  onDeleteWorkspace,
  onDeleteWorktree,
}: SidebarMenuHandlers) {
  const { t } = useTranslation("app");
  const showThreadMenu = useCallback(
    async (
      event: MouseEvent,
      workspaceId: string,
      threadId: string,
      canPin: boolean,
    ) => {
      event.preventDefault();
      event.stopPropagation();
      const renameItem = await MenuItem.new({
        text: t("sidebar.menu.rename"),
        action: () => onRenameThread(workspaceId, threadId),
      });
      const syncItem = await MenuItem.new({
        text: t("sidebar.menu.sync"),
        action: () => onSyncThread(workspaceId, threadId),
      });
      const archiveItem = await MenuItem.new({
        text: t("sidebar.menu.archive"),
        action: () => onDeleteThread(workspaceId, threadId),
      });
      const copyItem = await MenuItem.new({
        text: t("sidebar.menu.copyId"),
        action: async () => {
          try {
            await navigator.clipboard.writeText(threadId);
          } catch {
            // Clipboard failures are non-fatal here.
          }
        },
      });
      const items = [renameItem, syncItem];
      if (canPin) {
        const isPinned = isThreadPinned(workspaceId, threadId);
        items.push(
          await MenuItem.new({
            text: isPinned ? t("sidebar.menu.unpin") : t("sidebar.menu.pin"),
            action: () => {
              if (isPinned) {
                onUnpinThread(workspaceId, threadId);
              } else {
                onPinThread(workspaceId, threadId);
              }
            },
          }),
        );
      }
      items.push(copyItem, archiveItem);
      const menu = await Menu.new({ items });
      const window = getCurrentWindow();
      const position = new LogicalPosition(event.clientX, event.clientY);
      await menu.popup(position, window);
    },
    [
      isThreadPinned,
      onDeleteThread,
      onPinThread,
      onRenameThread,
      onSyncThread,
      onUnpinThread,
      t,
    ],
  );

  const getWorkspaceActions = useCallback(
    (workspace: WorkspaceInfo): WorkspaceHoverAction[] => [
      {
        id: "new-agent",
        label: t("sidebar.workspace.newAgent"),
        onSelect: () => onAddAgent(workspace),
      },
      {
        id: "new-worktree-agent",
        label: t("sidebar.workspace.newWorktreeAgent"),
        onSelect: () => onAddWorktreeAgent(workspace),
      },
      {
        id: "new-clone-agent",
        label: t("sidebar.workspace.newCloneAgent"),
        onSelect: () => onAddCloneAgent(workspace),
      },
      {
        id: "edit",
        label: t("sidebar.menu.editWorkspace"),
        onSelect: () => onEditWorkspace(workspace.id),
      },
      {
        id: "reload",
        label: t("sidebar.menu.reload"),
        onSelect: () => onReloadWorkspaceThreads(workspace.id),
      },
      {
        id: "delete",
        label: t("sidebar.menu.delete"),
        onSelect: () => onDeleteWorkspace(workspace.id),
        destructive: true,
      },
    ],
    [
      onAddAgent,
      onAddWorktreeAgent,
      onAddCloneAgent,
      onEditWorkspace,
      onReloadWorkspaceThreads,
      onDeleteWorkspace,
      t,
    ],
  );

  const getRevealAction = useCallback(
    (workspace: WorkspaceInfo, kind: "worktree" | "clone"): WorkspaceHoverAction => {
      const fileManagerLabel = fileManagerName();
      return {
        id: "reveal",
        label: t("sidebar.menu.showIn", { fileManager: fileManagerLabel }),
        onSelect: async () => {
          if (!workspace.path) {
            return;
          }
          try {
            const { revealItemInDir } = await import(
              "@tauri-apps/plugin-opener"
            );
            await revealItemInDir(workspace.path);
          } catch (error) {
            const message = error instanceof Error ? error.message : String(error);
            pushErrorToast({
              title: t(
                kind === "worktree"
                  ? "sidebar.menu.showWorktreeFailed"
                  : "sidebar.menu.showCloneFailed",
                { fileManager: fileManagerLabel },
              ),
              message,
            });
            console.warn(`Failed to reveal ${kind}`, {
              message,
              workspaceId: workspace.id,
              path: workspace.path,
            });
          }
        },
      };
    },
    [t],
  );

  const getWorktreeActions = useCallback(
    (worktree: WorkspaceInfo): WorkspaceHoverAction[] => [
      {
        id: "reload",
        label: t("sidebar.menu.reload"),
        onSelect: () => onReloadWorkspaceThreads(worktree.id),
      },
      getRevealAction(worktree, "worktree"),
      {
        id: "delete",
        label: t("sidebar.menu.deleteWorktree"),
        onSelect: () => onDeleteWorktree(worktree.id),
        destructive: true,
      },
    ],
    [getRevealAction, onReloadWorkspaceThreads, onDeleteWorktree, t],
  );

  const getCloneActions = useCallback(
    (clone: WorkspaceInfo): WorkspaceHoverAction[] => [
      {
        id: "edit",
        label: t("sidebar.menu.editWorkspace"),
        onSelect: () => onEditWorkspace(clone.id),
      },
      {
        id: "reload",
        label: t("sidebar.menu.reload"),
        onSelect: () => onReloadWorkspaceThreads(clone.id),
      },
      getRevealAction(clone, "clone"),
      {
        id: "delete",
        label: t("sidebar.menu.deleteClone"),
        onSelect: () => onDeleteWorkspace(clone.id),
        destructive: true,
      },
    ],
    [getRevealAction, onEditWorkspace, onReloadWorkspaceThreads, onDeleteWorkspace, t],
  );

  const showWorkspaceMenu = useCallback(
    (event: MouseEvent, workspace: WorkspaceInfo) =>
      showWorkspaceActionsMenu(event, getWorkspaceActions(workspace)),
    [getWorkspaceActions],
  );

  const showWorktreeMenu = useCallback(
    (event: MouseEvent, worktree: WorkspaceInfo) =>
      showWorkspaceActionsMenu(event, getWorktreeActions(worktree)),
    [getWorktreeActions],
  );

  const showCloneMenu = useCallback(
    (event: MouseEvent, clone: WorkspaceInfo) =>
      showWorkspaceActionsMenu(event, getCloneActions(clone)),
    [getCloneActions],
  );

  return {
    showThreadMenu,
    showWorkspaceMenu,
    showWorktreeMenu,
    showCloneMenu,
    getWorkspaceActions,
    getWorktreeActions,
    getCloneActions,
  };
}

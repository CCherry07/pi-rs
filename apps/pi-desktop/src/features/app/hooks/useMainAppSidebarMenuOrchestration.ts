import { useAppMenuEvents } from "@app/hooks/useAppMenuEvents";
import { useMenuAcceleratorController } from "@app/hooks/useMenuAcceleratorController";
import { useSidebarLayoutActions } from "@app/hooks/useSidebarLayoutActions";
import { useWorkspaceCycling } from "@app/hooks/useWorkspaceCycling";

type UseMainAppSidebarMenuOrchestrationArgs = {
  sidebarActions: Parameters<typeof useSidebarLayoutActions>[0];
  workspaceCycling: Parameters<typeof useWorkspaceCycling>[0];
  appMenu: Omit<
    Parameters<typeof useAppMenuEvents>[0],
    | "onOpenSettings"
    | "onCycleAgent"
    | "onCycleWorkspace"
    | "onAddWorkspace"
    | "onAddWorkspaceFromUrl"
    | "onAddAgent"
    | "onAddWorktreeAgent"
    | "onAddCloneAgent"
  > & {
    onAddWorkspace: () => void;
    onAddWorkspaceFromUrl: () => void;
    onAddAgent: NonNullable<Parameters<typeof useAppMenuEvents>[0]["onAddAgent"]>;
    onAddWorktreeAgent: NonNullable<
      Parameters<typeof useAppMenuEvents>[0]["onAddWorktreeAgent"]
    >;
    onAddCloneAgent: NonNullable<
      Parameters<typeof useAppMenuEvents>[0]["onAddCloneAgent"]
    >;
  };
  appSettings: Parameters<typeof useMenuAcceleratorController>[0]["appSettings"];
  onDebug: Parameters<typeof useMenuAcceleratorController>[0]["onDebug"];
};

export function useMainAppSidebarMenuOrchestration({
  sidebarActions,
  workspaceCycling,
  appMenu,
  appSettings,
  onDebug,
}: UseMainAppSidebarMenuOrchestrationArgs) {
  const sidebarHandlers = useSidebarLayoutActions(sidebarActions);
  const { handleCycleAgent, handleCycleWorkspace } = useWorkspaceCycling(workspaceCycling);

  useAppMenuEvents({
    ...appMenu,
    onAddWorkspace: () => {
      appMenu.onAddWorkspace();
    },
    onAddWorkspaceFromUrl: () => {
      appMenu.onAddWorkspaceFromUrl();
    },
    onAddAgent: (workspace) => {
      void appMenu.onAddAgent(workspace);
    },
    onAddWorktreeAgent: (workspace) => {
      void appMenu.onAddWorktreeAgent(workspace);
    },
    onAddCloneAgent: (workspace) => {
      void appMenu.onAddCloneAgent(workspace);
    },
    onOpenSettings: sidebarHandlers.onOpenSettings,
    onCycleAgent: handleCycleAgent,
    onCycleWorkspace: handleCycleWorkspace,
  });

  useMenuAcceleratorController({ appSettings, onDebug });

  return sidebarHandlers;
}

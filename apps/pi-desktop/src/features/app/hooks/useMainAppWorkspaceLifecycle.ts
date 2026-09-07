import { useWindowDrag } from "@/features/layout/hooks/useWindowDrag";
import { useWorkspaceRestore } from "@/features/workspaces/hooks/useWorkspaceRestore";
import { useTabActivationGuard } from "@app/hooks/useTabActivationGuard";
import type { WorkspaceInfo } from "@/types";

type UseMainAppWorkspaceLifecycleArgs = {
  activeTab: "home" | "projects" | "chat" | "git" | "log";
  isTablet: boolean;
  setActiveTab: (tab: "home" | "projects" | "chat" | "git" | "log") => void;
  workspaces: WorkspaceInfo[];
  hasLoaded: boolean;
  listThreadsForWorkspaces: (workspaces: WorkspaceInfo[]) => Promise<void>;
};

export function useMainAppWorkspaceLifecycle({
  activeTab,
  isTablet,
  setActiveTab,
  workspaces,
  hasLoaded,
  listThreadsForWorkspaces,
}: UseMainAppWorkspaceLifecycleArgs) {
  useTabActivationGuard({
    activeTab,
    isTablet,
    setActiveTab,
  });

  useWindowDrag("titlebar");

  useWorkspaceRestore({
    workspaces,
    hasLoaded,
    listThreadsForWorkspaces,
  });
}

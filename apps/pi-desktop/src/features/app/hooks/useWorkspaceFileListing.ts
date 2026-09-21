import { useEffect, useState } from "react";
import type { DebugEntry, FileMention, WorkspaceFileListing, WorkspaceInfo } from "../../../types";
import { useWorkspaceFiles } from "../../workspaces/hooks/useWorkspaceFiles";

type FilePanelMode = "git" | "files" | "prompts";
type TabKey = "home" | "projects" | "chat" | "git" | "log";
type TabletTabKey = "chat" | "git" | "log";

type UseWorkspaceFileListingArgs = {
  activeWorkspace: WorkspaceInfo | null;
  threadId?: string | null;
  activeWorkspaceId: string | null;
  filePanelMode: FilePanelMode;
  isCompact: boolean;
  isTablet: boolean;
  activeTab: TabKey;
  tabletTab: TabletTabKey;
  rightPanelCollapsed: boolean;
  hasComposerSurface: boolean;
  onDebug?: (entry: DebugEntry) => void;
};

type UseWorkspaceFileListingResult = {
  files: FileMention[];
  listing: WorkspaceFileListing | null;
  error: string | null;
  refreshFiles: () => Promise<void>;
  isLoading: boolean;
  setFileAutocompleteActive: (active: boolean) => void;
};

export function useWorkspaceFileListing({
  activeWorkspace,
  threadId,
  activeWorkspaceId,
  filePanelMode,
  isCompact,
  isTablet,
  activeTab,
  tabletTab,
  rightPanelCollapsed,
  hasComposerSurface,
  onDebug,
}: UseWorkspaceFileListingArgs): UseWorkspaceFileListingResult {
  const [fileAutocompleteActive, setFileAutocompleteActive] = useState(false);

  const compactTab = isTablet ? tabletTab : activeTab;
  const filePanelVisible =
    filePanelMode === "files" &&
    (isCompact ? compactTab === "git" : !rightPanelCollapsed);
  const shouldFetchFiles =
    Boolean(activeWorkspace) && (filePanelMode === "files" || fileAutocompleteActive);

  useEffect(() => {
    if (!activeWorkspaceId) {
      setFileAutocompleteActive(false);
    }
  }, [activeWorkspaceId]);

  useEffect(() => {
    if (!hasComposerSurface) {
      setFileAutocompleteActive(false);
    }
  }, [hasComposerSurface]);

  const { files, listing, error, refreshFiles, isLoading } = useWorkspaceFiles({
    activeWorkspace,
    threadId,
    onDebug,
    enabled: shouldFetchFiles,
    pollingEnabled: filePanelVisible,
  });

  return { files, listing, error, refreshFiles, isLoading, setFileAutocompleteActive };
}

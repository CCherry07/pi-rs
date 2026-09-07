import { useCallback, useEffect } from "react";
import i18n from "@/i18n";
import type { ConversationItem, DebugEntry, WorkspaceInfo } from "@/types";
import { useGitPanelController } from "@app/hooks/useGitPanelController";
import { useGitHubPanelController } from "@app/hooks/useGitHubPanelController";
import { useGitCommitController } from "@app/hooks/useGitCommitController";
import { useGitRootSelection } from "@app/hooks/useGitRootSelection";
import { useGitRemote } from "@/features/git/hooks/useGitRemote";
import { useGitRepoScan } from "@/features/git/hooks/useGitRepoScan";
import { useGitActions } from "@/features/git/hooks/useGitActions";
import { useGitBranches } from "@/features/git/hooks/useGitBranches";
import { useSyncSelectedDiffPath } from "@app/hooks/useSyncSelectedDiffPath";

type UseMainAppGitStateOptions = {
  activeWorkspace: WorkspaceInfo | null;
  activeWorkspaceId: string | null;
  activeItems: ConversationItem[];
  activeTab: "home" | "projects" | "chat" | "git" | "log";
  tabletTab: "chat" | "git" | "log";
  isCompact: boolean;
  isTablet: boolean;
  setActiveTab: (tab: "home" | "projects" | "chat" | "git" | "log") => void;
  appSettings: {
    preloadGitDiffs: boolean;
    gitDiffIgnoreWhitespaceChanges: boolean;
    splitChatDiffView: boolean;
  };
  addDebugEntry: (entry: DebugEntry) => void;
  updateWorkspaceSettings: Parameters<typeof useGitRootSelection>[0]["updateWorkspaceSettings"];
  commitMessageModelId: string | null;
};

type GitStatusSummary = {
  error: unknown;
  files: Array<unknown>;
};

function buildGitStatusText(gitStatus: GitStatusSummary) {
  if (gitStatus.error) {
    return i18n.t("statusSummary.unavailable", { ns: "git" });
  }
  return gitStatus.files.length > 0
    ? i18n.t("statusSummary.changed", {
        ns: "git",
        count: gitStatus.files.length,
      })
    : i18n.t("statusSummary.clean", { ns: "git" });
}

function resolveShouldLoadGitHubPanelData({
  gitPanelMode,
  shouldLoadDiffs,
  diffSource,
}: {
  gitPanelMode: "diff" | "issues" | "log" | "perFile" | "prs";
  shouldLoadDiffs: boolean;
  diffSource: "commit" | "local" | "perFile" | "pr";
}) {
  return (
    gitPanelMode === "issues" ||
    gitPanelMode === "prs" ||
    (shouldLoadDiffs && diffSource === "pr")
  );
}

function useMainAppGitBranchActions({
  activeWorkspace,
  addDebugEntry,
  refreshGitStatus,
  refreshGitLog,
  currentBranch,
}: {
  activeWorkspace: WorkspaceInfo | null;
  addDebugEntry: (entry: DebugEntry) => void;
  refreshGitStatus: () => void;
  refreshGitLog: () => void;
  currentBranch: string | null;
}) {
  const { branches, checkoutBranch, checkoutPullRequest, createBranch } = useGitBranches({
    activeWorkspace,
    onDebug: addDebugEntry,
  });

  const alertError = useCallback((error: unknown) => {
    alert(error instanceof Error ? error.message : String(error));
  }, []);

  const handleCheckoutBranch = useCallback(
    async (name: string) => {
      await checkoutBranch(name);
      refreshGitStatus();
    },
    [checkoutBranch, refreshGitStatus],
  );

  const handleCheckoutPullRequest = useCallback(
    async (prNumber: number) => {
      try {
        await checkoutPullRequest(prNumber);
        await Promise.resolve(refreshGitStatus());
        await Promise.resolve(refreshGitLog());
      } catch (error) {
        alertError(error);
      }
    },
    [alertError, checkoutPullRequest, refreshGitLog, refreshGitStatus],
  );

  const handleCreateBranch = useCallback(
    async (name: string) => {
      await createBranch(name);
      refreshGitStatus();
    },
    [createBranch, refreshGitStatus],
  );

  return {
    branches,
    currentBranch,
    isBranchSwitcherEnabled: Boolean(activeWorkspace) && activeWorkspace?.kind !== "worktree",
    handleCheckoutBranch,
    handleCheckoutPullRequest,
    handleCreateBranch,
  };
}

export function useMainAppGitState({
  activeWorkspace,
  activeWorkspaceId,
  activeItems,
  activeTab,
  tabletTab,
  isCompact,
  isTablet,
  setActiveTab,
  appSettings,
  addDebugEntry,
  updateWorkspaceSettings,
  commitMessageModelId,
}: UseMainAppGitStateOptions) {
  const alertError = useCallback((error: unknown) => {
    alert(error instanceof Error ? error.message : String(error));
  }, []);

  const {
    gitIssues,
    gitIssuesTotal,
    gitIssuesLoading,
    gitIssuesError,
    gitPullRequests,
    gitPullRequestsTotal,
    gitPullRequestsLoading,
    gitPullRequestsError,
    gitPullRequestDiffs,
    gitPullRequestDiffsLoading,
    gitPullRequestDiffsError,
    gitPullRequestComments,
    gitPullRequestCommentsLoading,
    gitPullRequestCommentsError,
    handleGitIssuesChange,
    handleGitPullRequestsChange,
    handleGitPullRequestDiffsChange,
    handleGitPullRequestCommentsChange,
    resetGitHubPanelState,
  } = useGitHubPanelController();

  useEffect(() => {
    resetGitHubPanelState();
  }, [activeWorkspaceId, resetGitHubPanelState]);

  const { remote: gitRemoteUrl, refresh: refreshGitRemote } = useGitRemote(activeWorkspace);
  const {
    repos: gitRootCandidates,
    isLoading: gitRootScanLoading,
    error: gitRootScanError,
    depth: gitRootScanDepth,
    hasScanned: gitRootScanHasScanned,
    scan: scanGitRoots,
    setDepth: setGitRootScanDepth,
    clear: clearGitRootCandidates,
  } = useGitRepoScan(activeWorkspace);

  const {
    centerMode,
    setCenterMode,
    selectedDiffPath,
    setSelectedDiffPath,
    diffScrollRequestId,
    gitPanelMode,
    setGitPanelMode,
    gitDiffViewStyle,
    setGitDiffViewStyle,
    filePanelMode,
    setFilePanelMode,
    selectedPullRequest,
    setSelectedPullRequest,
    selectedCommitSha,
    setSelectedCommitSha,
    diffSource,
    setDiffSource,
    gitStatus,
    refreshGitStatus,
    queueGitStatusRefresh,
    refreshGitDiffs,
    gitLogEntries,
    gitLogTotal,
    gitLogAhead,
    gitLogBehind,
    gitLogAheadEntries,
    gitLogBehindEntries,
    gitLogUpstream,
    gitLogLoading,
    gitLogError,
    refreshGitLog,
    gitCommitDiffs,
    shouldLoadDiffs,
    activeDiffs,
    activeDiffLoading,
    activeDiffError,
    perFileDiffGroups,
    handleSelectDiff,
    handleSelectPerFileDiff,
    handleSelectCommit,
    handleActiveDiffPath,
    handleGitPanelModeChange,
    activeWorkspaceIdRef,
    activeWorkspaceRef,
  } = useGitPanelController({
    activeWorkspace,
    activeItems,
    gitDiffPreloadEnabled: appSettings.preloadGitDiffs,
    gitDiffIgnoreWhitespaceChanges: appSettings.gitDiffIgnoreWhitespaceChanges,
    splitChatDiffView: appSettings.splitChatDiffView,
    isCompact,
    isTablet,
    activeTab,
    tabletTab,
    setActiveTab,
    prDiffs: gitPullRequestDiffs,
    prDiffsLoading: gitPullRequestDiffsLoading,
    prDiffsError: gitPullRequestDiffsError,
  });

  const shouldLoadGitHubPanelData = resolveShouldLoadGitHubPanelData({
    gitPanelMode,
    shouldLoadDiffs,
    diffSource,
  });

  const {
    branches,
    currentBranch,
    isBranchSwitcherEnabled,
    handleCheckoutBranch,
    handleCheckoutPullRequest,
    handleCreateBranch,
  } = useMainAppGitBranchActions({
    activeWorkspace,
    addDebugEntry,
    refreshGitStatus,
    refreshGitLog,
    currentBranch: gitStatus.branchName ?? null,
  });

  const {
    applyWorktreeChanges: handleApplyWorktreeChanges,
    createGitHubRepo: handleCreateGitHubRepo,
    createGitHubRepoLoading,
    initGitRepo: handleInitGitRepo,
    initGitRepoLoading,
    revertAllGitChanges: handleRevertAllGitChanges,
    revertGitFile: handleRevertGitFile,
    stageGitAll: handleStageGitAll,
    stageGitFile: handleStageGitFile,
    unstageGitFile: handleUnstageGitFile,
    worktreeApplyError,
    worktreeApplyLoading,
    worktreeApplySuccess,
  } = useGitActions({
    activeWorkspace,
    onRefreshGitStatus: refreshGitStatus,
    onRefreshGitDiffs: refreshGitDiffs,
    onClearGitRootCandidates: clearGitRootCandidates,
    onError: alertError,
  });

  const { activeGitRoot, handleSetGitRoot, handlePickGitRoot } = useGitRootSelection({
    activeWorkspace,
    updateWorkspaceSettings,
    clearGitRootCandidates,
    refreshGitStatus,
  });

  const fileStatus = buildGitStatusText(gitStatus);

  useSyncSelectedDiffPath({
    diffSource,
    centerMode,
    gitPullRequestDiffs,
    gitCommitDiffs,
    perFileDiffGroups,
    selectedDiffPath,
    setSelectedDiffPath,
  });

  const {
    commitMessage,
    commitMessageLoading,
    commitMessageError,
    commitLoading,
    pullLoading,
    fetchLoading,
    pushLoading,
    syncLoading,
    commitError,
    pullError,
    fetchError,
    pushError,
    syncError,
    onCommitMessageChange: handleCommitMessageChange,
    onGenerateCommitMessage: handleGenerateCommitMessage,
    onCommit: handleCommit,
    onCommitAndPush: handleCommitAndPush,
    onCommitAndSync: handleCommitAndSync,
    onPull: handlePull,
    onFetch: handleFetch,
    onPush: handlePush,
    onSync: handleSync,
  } = useGitCommitController({
    activeWorkspace,
    activeWorkspaceId,
    activeWorkspaceIdRef,
    commitMessageModelId,
    gitStatus,
    refreshGitStatus,
    refreshGitLog,
  });

  return {
    activeWorkspaceRef,
    activeWorkspaceIdRef,
    queueGitStatusRefresh,
    alertError,
    centerMode,
    setCenterMode,
    selectedDiffPath,
    setSelectedDiffPath,
    diffScrollRequestId,
    gitPanelMode,
    setGitPanelMode,
    gitDiffViewStyle,
    setGitDiffViewStyle,
    filePanelMode,
    setFilePanelMode,
    selectedPullRequest,
    setSelectedPullRequest,
    selectedCommitSha,
    setSelectedCommitSha,
    diffSource,
    setDiffSource,
    gitStatus,
    refreshGitStatus,
    refreshGitDiffs,
    gitLogEntries,
    gitLogTotal,
    gitLogAhead,
    gitLogBehind,
    gitLogAheadEntries,
    gitLogBehindEntries,
    gitLogUpstream,
    gitLogLoading,
    gitLogError,
    refreshGitLog,
    shouldLoadDiffs,
    activeDiffs,
    activeDiffLoading,
    activeDiffError,
    perFileDiffGroups,
    handleSelectDiff,
    handleSelectPerFileDiff,
    handleSelectCommit,
    handleActiveDiffPath,
    handleGitPanelModeChange,
    shouldLoadGitHubPanelData,
    gitIssues,
    gitIssuesTotal,
    gitIssuesLoading,
    gitIssuesError,
    gitPullRequests,
    gitPullRequestsTotal,
    gitPullRequestsLoading,
    gitPullRequestsError,
    gitPullRequestDiffs,
    gitPullRequestComments,
    gitPullRequestCommentsLoading,
    gitPullRequestCommentsError,
    handleGitIssuesChange,
    handleGitPullRequestsChange,
    handleGitPullRequestDiffsChange,
    handleGitPullRequestCommentsChange,
    gitRemoteUrl,
    refreshGitRemote,
    gitRootCandidates,
    gitRootScanLoading,
    gitRootScanError,
    gitRootScanDepth,
    gitRootScanHasScanned,
    scanGitRoots,
    setGitRootScanDepth,
    branches,
    currentBranch,
    isBranchSwitcherEnabled,
    handleCheckoutBranch,
    handleCheckoutPullRequest,
    handleCreateBranch,
    handleApplyWorktreeChanges,
    handleCreateGitHubRepo,
    createGitHubRepoLoading,
    handleInitGitRepo,
    initGitRepoLoading,
    handleRevertAllGitChanges,
    handleRevertGitFile,
    handleStageGitAll,
    handleStageGitFile,
    handleUnstageGitFile,
    worktreeApplyError,
    worktreeApplyLoading,
    worktreeApplySuccess,
    activeGitRoot,
    handleSetGitRoot,
    handlePickGitRoot,
    fileStatus,
    commitMessage,
    commitMessageLoading,
    commitMessageError,
    commitLoading,
    pullLoading,
    fetchLoading,
    pushLoading,
    syncLoading,
    commitError,
    pullError,
    fetchError,
    pushError,
    syncError,
    handleCommitMessageChange,
    handleGenerateCommitMessage,
    handleCommit,
    handleCommitAndPush,
    handleCommitAndSync,
    handlePull,
    handleFetch,
    handlePush,
    handleSync,
  };
}

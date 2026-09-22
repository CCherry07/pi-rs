import { useCallback, useEffect } from "react";
import i18n from "@/i18n";
import type { ConversationItem, DebugEntry, WorkspaceInfo } from "@/types";
import { useGitPanelController } from "@app/hooks/useGitPanelController";
import { useGitHubPanelController } from "@app/hooks/useGitHubPanelController";
import { useGitCommitController } from "@app/hooks/useGitCommitController";
import { useGitCheckouts } from "@/features/git/hooks/useGitCheckouts";
import { gitScopeKey, isManagedGitCheckout } from "@/features/git/gitContext";
import { useWorktreeDelivery } from "@/features/workspaces/hooks/useWorktreeDelivery";
import { useGitRemote } from "@/features/git/hooks/useGitRemote";
import { useGitActions } from "@/features/git/hooks/useGitActions";
import { useGitBranches } from "@/features/git/hooks/useGitBranches";
import { useSyncSelectedDiffPath } from "@app/hooks/useSyncSelectedDiffPath";

type UseMainAppGitStateOptions = {
  activeWorkspace: WorkspaceInfo | null;
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
  activeThreadId: string | null;
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
  const { branches, refreshBranches, checkoutBranch, checkoutPullRequest, createBranch } = useGitBranches({
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
    refreshBranches,
    currentBranch,
    isBranchSwitcherEnabled: Boolean(activeWorkspace) && !isManagedGitCheckout(activeWorkspace),
    handleCheckoutBranch,
    handleCheckoutPullRequest,
    handleCreateBranch,
  };
}

export function useMainAppGitState({
  activeWorkspace: projectWorkspace,
  activeItems,
  activeTab,
  tabletTab,
  isCompact,
  isTablet,
  setActiveTab,
  appSettings,
  addDebugEntry,
  activeThreadId,
  commitMessageModelId,
}: UseMainAppGitStateOptions) {
  const repositories = useGitCheckouts(projectWorkspace, activeThreadId);
  const activeWorkspace = repositories.gitWorkspace;
  const scopeKey = gitScopeKey(activeWorkspace);
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
  }, [scopeKey, resetGitHubPanelState]);

  const { remote: gitRemoteUrl, refresh: refreshGitRemote } = useGitRemote(activeWorkspace);
  const gitRootCandidates = repositories.options.map((option) => option.label);
  const gitRootScanLoading = repositories.isLoading;
  const gitRootScanError = repositories.error;
  const gitRootScanDepth = repositories.depth;
  const gitRootScanHasScanned = repositories.hasScanned;
  const scanGitRoots = repositories.scan;
  const setGitRootScanDepth = repositories.setDepth;
  const clearGitRootCandidates = repositories.refresh;

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
    projectWorkspace,
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
    refreshBranches,
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

  const refreshDeliveryGitData = useCallback(() => {
    refreshGitStatus();
    refreshGitDiffs();
    refreshGitLog();
    void refreshBranches();
  }, [refreshBranches, refreshGitDiffs, refreshGitLog, refreshGitStatus]);
  const worktreeDelivery = useWorktreeDelivery(projectWorkspace, activeThreadId, scopeKey, refreshDeliveryGitData);

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

  const activeGitRoot = repositories.workdir;
  const handleSetGitRoot = (path: string | null) => {
    const option = repositories.options.find((option) => option.label === path) ?? repositories.options[0];
    if (option) repositories.select(option.value);
  };

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
      commitMessageModelId,
    gitStatus,
    refreshGitStatus,
    refreshGitLog,
  });

  return {
    gitWorkspace: activeWorkspace,
    repositories,
    worktreeDelivery,
    canApplyWorktree: isManagedGitCheckout(activeWorkspace),
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

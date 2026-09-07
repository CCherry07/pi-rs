import type { RefObject } from "react";
import i18n from "@/i18n";
import type { AppSettings, ComposerEditorSettings, WorkspaceInfo } from "@/types";
import type { ThreadState } from "@/features/threads/hooks/useThreadsReducer";
import type { WorkspaceLaunchScriptsState } from "@app/hooks/useWorkspaceLaunchScripts";
import type { useMainAppComposerWorkspaceState } from "@app/hooks/useMainAppComposerWorkspaceState";
import type { useMainAppDisplayNodes } from "@app/hooks/useMainAppDisplayNodes";
import type { useMainAppGitState } from "@app/hooks/useMainAppGitState";
import type { useMainAppPromptActions } from "@app/hooks/useMainAppPromptActions";
import type { useMainAppSidebarMenuOrchestration } from "@app/hooks/useMainAppSidebarMenuOrchestration";
import type { useMainAppWorktreeState } from "@app/hooks/useMainAppWorktreeState";
import type { LayoutNodesOptions } from "@/features/layout/hooks/layoutNodes/types";

type SidebarProps = LayoutNodesOptions["primary"]["sidebarProps"];
type ComposerProps = NonNullable<LayoutNodesOptions["primary"]["composerProps"]>;
type MainHeaderProps = NonNullable<LayoutNodesOptions["primary"]["mainHeaderProps"]>;
type GitDiffPanelProps = LayoutNodesOptions["git"]["gitDiffPanelProps"];
const THREAD_REFRESH_POLL_INTERVAL_MS = 5000;

type UseMainAppLayoutSurfacesArgs = {
  appSettings: Pick<
    AppSettings,
    | "composerCodeBlockCopyUseModifier"
    | "showMessageFilePath"
    | "openAppTargets"
    | "selectedOpenAppId"
    | "followUpMessageBehavior"
    | "dictationEnabled"
    | "splitChatDiffView"
    | "gitDiffIgnoreWhitespaceChanges"
  >;
  workspaces: WorkspaceInfo[];
  groupedWorkspaces: Array<{ id: string | null; name: string; workspaces: WorkspaceInfo[] }>;
  workspaceGroupsCount: number;
  deletingWorktreeIds: Set<string>;
  newAgentDraftWorkspaceId: string | null;
  startingDraftThreadWorkspaceId: string | null;
  threadsByWorkspace: SidebarProps["threadsByWorkspace"];
  threadParentById: SidebarProps["threadParentById"];
  threadStatusById: ThreadState["threadStatusById"];
  threadResumeLoadingById: Record<string, boolean>;
  threadListLoadingByWorkspace: SidebarProps["threadListLoadingByWorkspace"];
  threadListPagingByWorkspace: SidebarProps["threadListPagingByWorkspace"];
  threadListCursorByWorkspace: SidebarProps["threadListCursorByWorkspace"];
  pinnedThreadsVersion: number;
  threadListSortKey: SidebarProps["threadListSortKey"];
  onSetThreadListSortKey: SidebarProps["onSetThreadListSortKey"];
  threadListOrganizeMode: SidebarProps["threadListOrganizeMode"];
  onSetThreadListOrganizeMode: SidebarProps["onSetThreadListOrganizeMode"];
  onRefreshAllThreads: SidebarProps["onRefreshAllThreads"];
  activeWorkspace: WorkspaceInfo | null;
  activeWorkspaceId: string | null;
  activeThreadId: string | null;
  activeThreadReadOnly: boolean;
  activeItems: LayoutNodesOptions["primary"]["messagesProps"]["items"];
  onPlanAccept: LayoutNodesOptions["primary"]["messagesProps"]["onPlanAccept"];
  onPlanSubmitChanges: LayoutNodesOptions["primary"]["messagesProps"]["onPlanSubmitChanges"];
  activePlan: LayoutNodesOptions["secondary"]["planPanelProps"]["plan"];
  activeTokenUsage: ComposerProps["contextUsage"];
  latestAgentRuns: LayoutNodesOptions["primary"]["homeProps"]["latestAgentRuns"];
  isLoadingLatestAgents: LayoutNodesOptions["primary"]["homeProps"]["isLoadingLatestAgents"];
  gitState: ReturnType<typeof useMainAppGitState>;
  composerWorkspaceState: ReturnType<typeof useMainAppComposerWorkspaceState>;
  promptActions: ReturnType<typeof useMainAppPromptActions>;
  worktreeState: ReturnType<typeof useMainAppWorktreeState>;
  sidebarHandlers: ReturnType<typeof useMainAppSidebarMenuOrchestration>;
  displayNodes: ReturnType<typeof useMainAppDisplayNodes>;
  threadPinning: Pick<
    SidebarProps,
    "pinThread" | "unpinThread" | "isThreadPinned" | "getPinTimestamp"
  >;
  workspaceDrop: {
    workspaceDropTargetRef: SidebarProps["workspaceDropTargetRef"];
    isWorkspaceDropActive: SidebarProps["isWorkspaceDropActive"];
    workspaceDropText: SidebarProps["workspaceDropText"];
    onWorkspaceDragOver: SidebarProps["onWorkspaceDragOver"];
    onWorkspaceDragEnter: SidebarProps["onWorkspaceDragEnter"];
    onWorkspaceDragLeave: SidebarProps["onWorkspaceDragLeave"];
    onWorkspaceDrop: SidebarProps["onWorkspaceDrop"];
  };
  threadNavigation: {
    exitDiffView: () => void;
    clearDraftState: () => void;
    selectWorkspace: (workspaceId: string) => void;
    setActiveThreadId: (threadId: string | null, workspaceId: string) => void;
    resetPullRequestSelection: () => void;
    selectHome: () => void;
  };
  pullRequestComposer: {
    handleSelectPullRequest: NonNullable<GitDiffPanelProps["onSelectPullRequest"]>;
  };
  dictationUi: {
    onOpenDictationSettings: ComposerProps["onOpenDictationSettings"];
    dictationTranscript: ComposerProps["dictationTranscript"];
    dictationError: ComposerProps["dictationError"];
    dictationHint: ComposerProps["dictationHint"];
  };
  openAppIconById: MainHeaderProps["openAppIconById"];
  openInitGitRepoPrompt: GitDiffPanelProps["onInitGitRepo"];
  handleAddWorkspace: () => void;
  openWorkspaceFromUrlPrompt: () => void;
  handleAddAgent: SidebarProps["onAddAgent"];
  handleAddWorktreeAgent: SidebarProps["onAddWorktreeAgent"];
  handleAddCloneAgent: SidebarProps["onAddCloneAgent"];
  handleOpenThreadLink: LayoutNodesOptions["primary"]["messagesProps"]["onOpenThreadLink"];
  handleSelectOpenAppId: MainHeaderProps["onSelectOpenAppId"];
  handleCopyThread: MainHeaderProps["onCopyThread"];
  handleToggleTerminalWithFocus: MainHeaderProps["onToggleTerminal"];
  launchScriptState: {
    launchScript: string | null;
    editorOpen: boolean;
    draftScript: string;
    isSaving: boolean;
    error: string | null;
    onRunLaunchScript: () => void;
    onOpenEditor: () => void;
    onCloseEditor: () => void;
    onDraftScriptChange: (value: string) => void;
    onSaveLaunchScript: () => void;
  };
  launchScriptsState: WorkspaceLaunchScriptsState | undefined;
  models: ComposerProps["models"];
  selectedModelId: ComposerProps["selectedModelId"];
  onSelectModel: ComposerProps["onSelectModel"];
  reasoningOptions: ComposerProps["reasoningOptions"];
  selectedEffort: ComposerProps["selectedEffort"];
  onSelectEffort: ComposerProps["onSelectEffort"];
  selectedServiceTier: ComposerProps["selectedServiceTier"];
  reasoningSupported: boolean;
  skills: ComposerProps["skills"];
  prompts: ComposerProps["prompts"];
  composerInputRef: RefObject<HTMLTextAreaElement | null>;
  composerEditorSettings: ComposerEditorSettings;
  composerEditorExpanded: boolean;
  onToggleComposerEditorExpanded: () => void;
  dictationReady: boolean;
  dictationState: ComposerProps["dictationState"];
  dictationLevel: number;
  onToggleDictation: () => void;
  onCancelDictation: (() => void) | undefined;
  clearDictationTranscript: NonNullable<ComposerProps["onDictationTranscriptHandled"]>;
  clearDictationError: () => void;
  clearDictationHint: () => void;
  handleComposerSendWithDraftStart: ComposerProps["onSend"];
  interruptTurn: () => void;
  terminalOpen: boolean;
  debugOpen: boolean;
  debugEntries: LayoutNodesOptions["secondary"]["debugPanelProps"]["entries"];
  terminalTabs: LayoutNodesOptions["secondary"]["terminalDockProps"]["terminals"];
  activeTerminalId: LayoutNodesOptions["secondary"]["terminalDockProps"]["activeTerminalId"];
  onSelectTerminal: LayoutNodesOptions["secondary"]["terminalDockProps"]["onSelectTerminal"];
  onNewTerminal: LayoutNodesOptions["secondary"]["terminalDockProps"]["onNewTerminal"];
  onCloseTerminal: LayoutNodesOptions["secondary"]["terminalDockProps"]["onCloseTerminal"];
  terminalState: LayoutNodesOptions["secondary"]["terminalState"];
  onClearDebug: () => void;
  onCopyDebug: () => void;
  onResizeDebug: LayoutNodesOptions["secondary"]["debugPanelProps"]["onResizeStart"];
  onResizeTerminal: LayoutNodesOptions["secondary"]["terminalDockProps"]["onResizeStart"];
  isCompact: boolean;
  isPhone: boolean;
  activeTab: LayoutNodesOptions["primary"]["tabBarProps"]["activeTab"];
  setActiveTab: (tab: "home" | "projects" | "chat" | "git" | "log") => void;
  tabletTab: LayoutNodesOptions["primary"]["tabletNavProps"]["activeTab"];
  showMobilePollingFetchStatus: boolean;
  appModalsAboutOpen: boolean;
  updaterState: LayoutNodesOptions["primary"]["updateToastProps"]["state"];
  startUpdate: LayoutNodesOptions["primary"]["updateToastProps"]["onUpdate"];
  dismissUpdate: LayoutNodesOptions["primary"]["updateToastProps"]["onDismiss"];
  postUpdateNotice: LayoutNodesOptions["primary"]["updateToastProps"]["postUpdateNotice"];
  dismissPostUpdateNotice: LayoutNodesOptions["primary"]["updateToastProps"]["onDismissPostUpdateNotice"];
  errorToasts: LayoutNodesOptions["primary"]["errorToastsProps"]["toasts"];
  dismissErrorToast: LayoutNodesOptions["primary"]["errorToastsProps"]["onDismiss"];
  showDebugButton: boolean;
  handleDebugClick: () => void;
};

type MainAppLayoutSurfacesContext = UseMainAppLayoutSurfacesArgs;

function buildPrimarySurface({
  appSettings,
  workspaces,
  groupedWorkspaces,
  workspaceGroupsCount,
  deletingWorktreeIds,
  newAgentDraftWorkspaceId,
  startingDraftThreadWorkspaceId,
  threadsByWorkspace,
  threadParentById,
  threadStatusById,
  threadResumeLoadingById,
  threadListLoadingByWorkspace,
  threadListPagingByWorkspace,
  threadListCursorByWorkspace,
  pinnedThreadsVersion,
  threadListSortKey,
  onSetThreadListSortKey,
  threadListOrganizeMode,
  onSetThreadListOrganizeMode,
  onRefreshAllThreads,
  activeWorkspace,
  activeWorkspaceId,
  activeThreadId,
  activeThreadReadOnly,
  activeItems,
  onPlanAccept,
  onPlanSubmitChanges,
  activeTokenUsage,
  latestAgentRuns,
  isLoadingLatestAgents,
  gitState,
  composerWorkspaceState,
  worktreeState,
  sidebarHandlers,
  displayNodes,
  threadPinning,
  workspaceDrop,
  threadNavigation,
  dictationUi,
  openAppIconById,
  handleAddWorkspace,
  openWorkspaceFromUrlPrompt,
  handleAddAgent,
  handleAddWorktreeAgent,
  handleAddCloneAgent,
  handleOpenThreadLink,
  handleSelectOpenAppId,
  handleCopyThread,
  handleToggleTerminalWithFocus,
  launchScriptState,
  launchScriptsState,
  models,
  selectedModelId,
  onSelectModel,
  reasoningOptions,
  selectedEffort,
  onSelectEffort,
  selectedServiceTier,
  reasoningSupported,
  skills,
  prompts,
  composerInputRef,
  composerEditorSettings,
  composerEditorExpanded,
  onToggleComposerEditorExpanded,
  dictationReady,
  dictationState,
  dictationLevel,
  onToggleDictation,
  onCancelDictation,
  clearDictationTranscript,
  clearDictationError,
  clearDictationHint,
  handleComposerSendWithDraftStart,
  interruptTurn,
  terminalOpen,
  isCompact,
  activeTab,
  setActiveTab,
  tabletTab,
  showMobilePollingFetchStatus,
  appModalsAboutOpen,
  updaterState,
  startUpdate,
  dismissUpdate,
  postUpdateNotice,
  dismissPostUpdateNotice,
  errorToasts,
  dismissErrorToast,
  showDebugButton,
  handleDebugClick,
}: MainAppLayoutSurfacesContext): LayoutNodesOptions["primary"] {
  return {
    sidebarProps: {
      workspaces,
      groupedWorkspaces,
      hasWorkspaceGroups: workspaceGroupsCount > 0,
      deletingWorktreeIds,
      newAgentDraftWorkspaceId,
      startingDraftThreadWorkspaceId,
      threadsByWorkspace,
      threadParentById,
      threadStatusById,
      threadListLoadingByWorkspace,
      threadListPagingByWorkspace,
      threadListCursorByWorkspace,
      pinnedThreadsVersion,
      threadListSortKey,
      onSetThreadListSortKey,
      threadListOrganizeMode,
      onSetThreadListOrganizeMode,
      onRefreshAllThreads,
      activeWorkspaceId,
      activeThreadId,
      onOpenSettings: sidebarHandlers.onOpenSettings,
      onOpenDebug: handleDebugClick,
      showDebugButton,
      onAddWorkspace: handleAddWorkspace,
      onSelectHome: sidebarHandlers.onSelectHome,
      onSelectWorkspace: sidebarHandlers.onSelectWorkspace,
      onAddAgent: handleAddAgent,
      onAddWorktreeAgent: handleAddWorktreeAgent,
      onAddCloneAgent: handleAddCloneAgent,
      onToggleWorkspaceCollapse: sidebarHandlers.onToggleWorkspaceCollapse,
      onSelectThread: sidebarHandlers.onSelectThread,
      onDeleteThread: sidebarHandlers.onDeleteThread,
      onSyncThread: sidebarHandlers.onSyncThread,
      pinThread: threadPinning.pinThread,
      unpinThread: threadPinning.unpinThread,
      isThreadPinned: threadPinning.isThreadPinned,
      getPinTimestamp: threadPinning.getPinTimestamp,
      onRenameThread: sidebarHandlers.onRenameThread,
      onDeleteWorkspace: sidebarHandlers.onDeleteWorkspace,
      onDeleteWorktree: sidebarHandlers.onDeleteWorktree,
      onLoadOlderThreads: sidebarHandlers.onLoadOlderThreads,
      onReloadWorkspaceThreads: sidebarHandlers.onReloadWorkspaceThreads,
      workspaceDropTargetRef: workspaceDrop.workspaceDropTargetRef,
      isWorkspaceDropActive: workspaceDrop.isWorkspaceDropActive,
      workspaceDropText: workspaceDrop.workspaceDropText,
      onWorkspaceDragOver: workspaceDrop.onWorkspaceDragOver,
      onWorkspaceDragEnter: workspaceDrop.onWorkspaceDragEnter,
      onWorkspaceDragLeave: workspaceDrop.onWorkspaceDragLeave,
      onWorkspaceDrop: workspaceDrop.onWorkspaceDrop,
    },
    messagesProps: {
      items: activeItems,
      threadId: activeThreadId ?? null,
      workspaceId: activeWorkspace?.id ?? null,
      workspacePath: activeWorkspace?.path ?? null,
      openTargets: appSettings.openAppTargets,
      selectedOpenAppId: appSettings.selectedOpenAppId,
      codeBlockCopyUseModifier: appSettings.composerCodeBlockCopyUseModifier,
      showMessageFilePath: appSettings.showMessageFilePath,
      onPlanAccept,
      onPlanSubmitChanges,
      onOpenThreadLink: handleOpenThreadLink,
      onQuoteMessage: composerWorkspaceState.canInsertComposerText
        ? composerWorkspaceState.handleInsertComposerText
        : undefined,
      isThinking: composerWorkspaceState.isProcessing,
      isLoadingMessages: activeThreadId
        ? threadResumeLoadingById[activeThreadId] ?? false
        : false,
      processingStartedAt: activeThreadId
        ? threadStatusById[activeThreadId]?.processingStartedAt ?? null
        : null,
      lastDurationMs: activeThreadId
        ? threadStatusById[activeThreadId]?.lastDurationMs ?? null
        : null,
      showPollingFetchStatus: showMobilePollingFetchStatus,
      pollingIntervalMs: THREAD_REFRESH_POLL_INTERVAL_MS,
    },
    composerProps: composerWorkspaceState.showComposer
      ? {
          onSend: handleComposerSendWithDraftStart,
          onStop: interruptTurn,
          disabled: activeThreadReadOnly,
          canStop: composerWorkspaceState.canInterrupt,
          onFileAutocompleteActiveChange: composerWorkspaceState.setFileAutocompleteActive,
          contextUsage: activeTokenUsage,
          queuedMessages: composerWorkspaceState.activeQueue,
          queuePausedReason: composerWorkspaceState.queuePausedReason,
          sendLabel: i18n.t("composer.send", { ns: "messages" }),
          steerAvailable: composerWorkspaceState.steerAvailable,
          followUpMessageBehavior: appSettings.followUpMessageBehavior,
          isProcessing: composerWorkspaceState.isProcessing,
          draftText: composerWorkspaceState.activeDraft,
          onDraftChange: composerWorkspaceState.handleDraftChange,
          attachedImages: composerWorkspaceState.activeImages,
          onPickImages: composerWorkspaceState.pickImages,
          onAttachImages: composerWorkspaceState.attachImages,
          onRemoveImage: composerWorkspaceState.removeImage,
          prefillDraft: composerWorkspaceState.prefillDraft,
          onPrefillHandled: (id) => {
            if (composerWorkspaceState.prefillDraft?.id === id) {
              composerWorkspaceState.setPrefillDraft(null);
            }
          },
          insertText: composerWorkspaceState.composerInsert,
          onInsertHandled: (id) => {
            if (composerWorkspaceState.composerInsert?.id === id) {
              composerWorkspaceState.setComposerInsert(null);
            }
          },
          onSteerQueued: composerWorkspaceState.handleSteerQueued,
          onEditQueued: composerWorkspaceState.handleEditQueued,
          onDeleteQueued: composerWorkspaceState.handleDeleteQueued,
          models,
          selectedModelId,
          onSelectModel,
          reasoningOptions,
          selectedEffort,
          onSelectEffort,
          selectedServiceTier,
          reasoningSupported,
          skills,
          prompts,
          files: composerWorkspaceState.files,
          textareaRef: composerInputRef,
          historyKey: activeWorkspace?.id ?? null,
          editorSettings: composerEditorSettings,
          editorExpanded: composerEditorExpanded,
          onToggleEditorExpanded: onToggleComposerEditorExpanded,
          dictationEnabled: appSettings.dictationEnabled && dictationReady,
          dictationState,
          dictationLevel,
          onToggleDictation,
          onCancelDictation,
          onOpenDictationSettings: dictationUi.onOpenDictationSettings,
          dictationTranscript: dictationUi.dictationTranscript,
          onDictationTranscriptHandled: (id) => {
            clearDictationTranscript?.(id);
          },
          dictationError: dictationUi.dictationError,
          onDismissDictationError: clearDictationError,
          dictationHint: dictationUi.dictationHint,
          onDismissDictationHint: clearDictationHint,
        }
      : null,
    updateToastProps: {
      state: appModalsAboutOpen ? { stage: "idle" as const } : updaterState,
      onUpdate: startUpdate,
      onDismiss: dismissUpdate,
      postUpdateNotice,
      onDismissPostUpdateNotice: dismissPostUpdateNotice,
    },
    errorToastsProps: {
      toasts: errorToasts,
      onDismiss: dismissErrorToast,
    },
    homeProps: {
      onAddWorkspace: handleAddWorkspace,
      onAddWorkspaceFromUrl: openWorkspaceFromUrlPrompt,
      latestAgentRuns,
      isLoadingLatestAgents,
      onSelectThread: (workspaceId, threadId) => {
        threadNavigation.exitDiffView();
        threadNavigation.clearDraftState();
        threadNavigation.selectWorkspace(workspaceId);
        threadNavigation.setActiveThreadId(threadId, workspaceId);
        if (isCompact) {
          setActiveTab("chat");
        }
      },
    },
    mainHeaderProps: activeWorkspace
      ? {
          workspace: activeWorkspace,
          parentName: worktreeState.activeParentWorkspace?.name ?? null,
          worktreeLabel: worktreeState.worktreeLabel,
          worktreeRename: worktreeState.worktreeRename ?? undefined,
          disableBranchMenu: worktreeState.isWorktreeWorkspace,
          parentPath: worktreeState.activeParentWorkspace?.path ?? null,
          worktreePath: worktreeState.isWorktreeWorkspace ? activeWorkspace.path : null,
          openTargets: appSettings.openAppTargets,
          openAppIconById,
          selectedOpenAppId: appSettings.selectedOpenAppId,
          onSelectOpenAppId: handleSelectOpenAppId,
          branchName: gitState.gitStatus.branchName || "unknown",
          branches: gitState.branches,
          onCheckoutBranch: gitState.handleCheckoutBranch,
          onCreateBranch: gitState.handleCreateBranch,
          canCopyThread: activeItems.length > 0,
          onCopyThread: handleCopyThread,
          onToggleTerminal: handleToggleTerminalWithFocus,
          isTerminalOpen: terminalOpen,
          showTerminalButton: !isCompact,
          showWorkspaceTools: !isCompact,
          launchScript: launchScriptState.launchScript,
          launchScriptEditorOpen: launchScriptState.editorOpen,
          launchScriptDraft: launchScriptState.draftScript,
          launchScriptSaving: launchScriptState.isSaving,
          launchScriptError: launchScriptState.error,
          onRunLaunchScript: launchScriptState.onRunLaunchScript,
          onOpenLaunchScriptEditor: launchScriptState.onOpenEditor,
          onCloseLaunchScriptEditor: launchScriptState.onCloseEditor,
          onLaunchScriptDraftChange: launchScriptState.onDraftScriptChange,
          onSaveLaunchScript: launchScriptState.onSaveLaunchScript,
          launchScriptsState,
          extraActionsNode: displayNodes.mainHeaderActionsNode,
        }
      : null,
    desktopTopbarProps: {
      showBackToChat: gitState.centerMode === "diff",
      onExitDiff: () => {
        gitState.setCenterMode("chat");
        gitState.setSelectedDiffPath(null);
      },
    },
    tabletNavProps: {
      activeTab: tabletTab,
      onSelect: setActiveTab,
    },
    tabBarProps: {
      activeTab,
      onSelect: (tab) => {
        if (tab === "home") {
          threadNavigation.resetPullRequestSelection();
          threadNavigation.clearDraftState();
          threadNavigation.selectHome();
          return;
        }
        setActiveTab(tab);
      },
    },
  };
}

function buildGitSurface({
  appSettings,
  activeWorkspace,
  gitState,
  composerWorkspaceState,
  promptActions,
  worktreeState,
  pullRequestComposer,
  openAppIconById,
  openInitGitRepoPrompt,
  handleSelectOpenAppId,
  prompts,
  isPhone,
}: MainAppLayoutSurfacesContext): LayoutNodesOptions["git"] {
  return {
    filePanelMode: gitState.filePanelMode,
    fileTreeProps: activeWorkspace
      ? {
          workspaceId: activeWorkspace.id,
          workspacePath: activeWorkspace.path,
          files: composerWorkspaceState.files,
          modifiedFiles: [
            ...new Set([
              ...gitState.gitStatus.stagedFiles.map((file) => file.path),
              ...gitState.gitStatus.unstagedFiles.map((file) => file.path),
            ]),
          ],
          isLoading: composerWorkspaceState.isFilesLoading,
          filePanelMode: gitState.filePanelMode,
          onFilePanelModeChange: gitState.setFilePanelMode,
          onInsertText: composerWorkspaceState.handleInsertComposerText,
          canInsertText: composerWorkspaceState.canInsertComposerText,
          openTargets: appSettings.openAppTargets,
          openAppIconById,
          selectedOpenAppId: appSettings.selectedOpenAppId,
          onSelectOpenAppId: handleSelectOpenAppId,
        }
      : null,
    promptPanelProps: {
      prompts,
      workspacePath: activeWorkspace?.path ?? null,
      filePanelMode: gitState.filePanelMode,
      onFilePanelModeChange: gitState.setFilePanelMode,
      onSendPrompt: composerWorkspaceState.handleSendPrompt,
      onSendPromptToNewAgent: promptActions.handleSendPromptToNewAgent,
      onCreatePrompt: promptActions.handleCreatePrompt,
      onUpdatePrompt: promptActions.handleUpdatePrompt,
      onDeletePrompt: promptActions.handleDeletePrompt,
      onMovePrompt: promptActions.handleMovePrompt,
      onRevealWorkspacePrompts: promptActions.handleRevealWorkspacePrompts,
      onRevealGeneralPrompts: promptActions.handleRevealGeneralPrompts,
      canRevealGeneralPrompts: Boolean(activeWorkspace),
    },
    gitDiffPanelProps: {
      workspaceId: activeWorkspace?.id ?? null,
      workspacePath: activeWorkspace?.path ?? null,
      mode: gitState.gitPanelMode,
      onModeChange: gitState.handleGitPanelModeChange,
      filePanelMode: gitState.filePanelMode,
      onFilePanelModeChange: gitState.setFilePanelMode,
      worktreeApplyLabel: i18n.t("actions.apply", { ns: "git" }),
      worktreeApplyTitle: worktreeState.activeParentWorkspace?.name
        ? i18n.t("actions.applyNamed", {
            ns: "git",
            name: worktreeState.activeParentWorkspace.name,
          })
        : i18n.t("actions.applyParent", { ns: "git" }),
      worktreeApplyLoading: worktreeState.isWorktreeWorkspace
        ? gitState.worktreeApplyLoading
        : false,
      worktreeApplyError: worktreeState.isWorktreeWorkspace
        ? gitState.worktreeApplyError
        : null,
      worktreeApplySuccess: worktreeState.isWorktreeWorkspace
        ? gitState.worktreeApplySuccess
        : false,
      onApplyWorktreeChanges: worktreeState.isWorktreeWorkspace
        ? gitState.handleApplyWorktreeChanges
        : undefined,
      branchName:
        gitState.gitStatus.branchName || i18n.t("branches.unknown", { ns: "git" }),
      totalAdditions: gitState.gitStatus.totalAdditions,
      totalDeletions: gitState.gitStatus.totalDeletions,
      fileStatus: gitState.fileStatus,
      perFileDiffGroups: gitState.perFileDiffGroups,
      error: gitState.gitStatus.error,
      logError: gitState.gitLogError,
      logLoading: gitState.gitLogLoading,
      stagedFiles: gitState.gitStatus.stagedFiles,
      unstagedFiles: gitState.gitStatus.unstagedFiles,
      onSelectFile:
        gitState.gitPanelMode === "perFile"
          ? gitState.handleSelectPerFileDiff
          : gitState.handleSelectDiff,
      logEntries: gitState.gitLogEntries,
      logTotal: gitState.gitLogTotal,
      logAhead: gitState.gitLogAhead,
      logBehind: gitState.gitLogBehind,
      logAheadEntries: gitState.gitLogAheadEntries,
      logBehindEntries: gitState.gitLogBehindEntries,
      logUpstream: gitState.gitLogUpstream,
      selectedCommitSha: gitState.selectedCommitSha,
      onSelectCommit: (entry) => {
        gitState.handleSelectCommit(entry.sha);
      },
      issues: gitState.gitIssues,
      issuesTotal: gitState.gitIssuesTotal,
      issuesLoading: gitState.gitIssuesLoading,
      issuesError: gitState.gitIssuesError,
      pullRequests: gitState.gitPullRequests,
      pullRequestsTotal: gitState.gitPullRequestsTotal,
      pullRequestsLoading: gitState.gitPullRequestsLoading,
      pullRequestsError: gitState.gitPullRequestsError,
      selectedPullRequest: gitState.selectedPullRequest?.number ?? null,
      onSelectPullRequest: (pullRequest) => {
        gitState.setSelectedCommitSha(null);
        pullRequestComposer.handleSelectPullRequest(pullRequest);
      },
      gitRemoteUrl: gitState.gitRemoteUrl,
      gitRoot: gitState.activeGitRoot,
      gitRootCandidates: gitState.gitRootCandidates,
      gitRootScanDepth: gitState.gitRootScanDepth,
      gitRootScanLoading: gitState.gitRootScanLoading,
      gitRootScanError: gitState.gitRootScanError,
      gitRootScanHasScanned: gitState.gitRootScanHasScanned,
      onGitRootScanDepthChange: gitState.setGitRootScanDepth,
      onScanGitRoots: gitState.scanGitRoots,
      onSelectGitRoot: (path) => {
        void gitState.handleSetGitRoot(path);
      },
      onClearGitRoot: () => {
        void gitState.handleSetGitRoot(null);
      },
      onPickGitRoot: gitState.handlePickGitRoot,
      onInitGitRepo: openInitGitRepoPrompt,
      initGitRepoLoading: gitState.initGitRepoLoading,
      onStageAllChanges: gitState.handleStageGitAll,
      onStageFile: gitState.handleStageGitFile,
      onUnstageFile: gitState.handleUnstageGitFile,
      onRevertFile: gitState.handleRevertGitFile,
      onRevertAllChanges: gitState.handleRevertAllGitChanges,
      commitMessage: gitState.commitMessage,
      commitMessageLoading: gitState.commitMessageLoading,
      commitMessageError: gitState.commitMessageError,
      onCommitMessageChange: gitState.handleCommitMessageChange,
      onGenerateCommitMessage: gitState.handleGenerateCommitMessage,
      onCommit: gitState.handleCommit,
      onCommitAndPush: gitState.handleCommitAndPush,
      onCommitAndSync: gitState.handleCommitAndSync,
      onPull: gitState.handlePull,
      onFetch: gitState.handleFetch,
      onPush: gitState.handlePush,
      onSync: gitState.handleSync,
      commitLoading: gitState.commitLoading,
      pullLoading: gitState.pullLoading,
      fetchLoading: gitState.fetchLoading,
      pushLoading: gitState.pushLoading,
      syncLoading: gitState.syncLoading,
      commitError: gitState.commitError,
      pullError: gitState.pullError,
      fetchError: gitState.fetchError,
      pushError: gitState.pushError,
      syncError: gitState.syncError,
      commitsAhead: gitState.gitLogAhead,
    },
    gitDiffViewerProps: {
      diffs: gitState.activeDiffs,
      selectedPath: gitState.selectedDiffPath,
      scrollRequestId: gitState.diffScrollRequestId,
      isLoading: gitState.activeDiffLoading,
      error: gitState.activeDiffError,
      ignoreWhitespaceChanges:
        appSettings.gitDiffIgnoreWhitespaceChanges && gitState.diffSource !== "pr",
      pullRequest: gitState.diffSource === "pr" ? gitState.selectedPullRequest : null,
      pullRequestComments:
        gitState.diffSource === "pr" ? gitState.gitPullRequestComments : [],
      pullRequestCommentsLoading: gitState.gitPullRequestCommentsLoading,
      pullRequestCommentsError: gitState.gitPullRequestCommentsError,
      onCheckoutPullRequest: (pullRequest) =>
        gitState.handleCheckoutPullRequest(pullRequest.number),
      canRevert: gitState.diffSource === "local",
      onRevertFile: gitState.handleRevertGitFile,
      onActivePathChange: gitState.handleActiveDiffPath,
      onInsertComposerText: composerWorkspaceState.canInsertComposerText
        ? composerWorkspaceState.handleInsertComposerText
        : undefined,
    },
    diffViewProps: {
      centerMode: gitState.centerMode,
      isPhone,
      splitChatDiffView: appSettings.splitChatDiffView,
      gitDiffViewStyle: gitState.gitDiffViewStyle,
    },
  };
}

function buildSecondarySurface({
  activePlan,
  composerWorkspaceState,
  gitState,
  terminalOpen,
  debugOpen,
  debugEntries,
  terminalTabs,
  activeTerminalId,
  onSelectTerminal,
  onNewTerminal,
  onCloseTerminal,
  terminalState,
  onClearDebug,
  onCopyDebug,
  onResizeDebug,
  onResizeTerminal,
  isPhone,
  setActiveTab,
}: MainAppLayoutSurfacesContext): LayoutNodesOptions["secondary"] {
  return {
    planPanelProps: {
      plan: activePlan,
      isProcessing: composerWorkspaceState.isProcessing,
    },
    terminalDockProps: {
      isOpen: terminalOpen,
      terminals: terminalTabs,
      activeTerminalId,
      onSelectTerminal,
      onNewTerminal,
      onCloseTerminal,
      onResizeStart: onResizeTerminal,
    },
    terminalState,
    debugPanelProps: {
      entries: debugEntries,
      isOpen: debugOpen,
      onClear: onClearDebug,
      onCopy: onCopyDebug,
      onResizeStart: onResizeDebug,
    },
    compactNavProps: {
      onGoProjects: () => setActiveTab("projects"),
      centerMode: gitState.centerMode,
      selectedDiffPath: gitState.selectedDiffPath,
      onBackFromDiff: () => {
        gitState.setCenterMode("chat");
      },
      onShowSelectedDiff: () => {
        const fallbackPath = gitState.selectedDiffPath ?? gitState.activeDiffs[0]?.path;

        if (!fallbackPath) {
          return;
        }

        if (!gitState.selectedDiffPath) {
          gitState.setSelectedDiffPath(fallbackPath);
        }

        gitState.setCenterMode("diff");
        if (isPhone) {
          setActiveTab("git");
        }
      },
      hasActiveGitDiffs: gitState.activeDiffs.length > 0,
    },
  };
}

export function useMainAppLayoutSurfaces({
  appSettings,
  workspaces,
  groupedWorkspaces,
  workspaceGroupsCount,
  deletingWorktreeIds,
  newAgentDraftWorkspaceId,
  startingDraftThreadWorkspaceId,
  threadsByWorkspace,
  threadParentById,
  threadStatusById,
  threadResumeLoadingById,
  threadListLoadingByWorkspace,
  threadListPagingByWorkspace,
  threadListCursorByWorkspace,
  pinnedThreadsVersion,
  threadListSortKey,
  onSetThreadListSortKey,
  threadListOrganizeMode,
  onSetThreadListOrganizeMode,
  onRefreshAllThreads,
  activeWorkspace,
  activeWorkspaceId,
  activeThreadId,
  activeThreadReadOnly,
  activeItems,
  onPlanAccept,
  onPlanSubmitChanges,
  activePlan,
  activeTokenUsage,
  latestAgentRuns,
  isLoadingLatestAgents,
  gitState,
  composerWorkspaceState,
  promptActions,
  worktreeState,
  sidebarHandlers,
  displayNodes,
  threadPinning,
  workspaceDrop,
  threadNavigation,
  pullRequestComposer,
  dictationUi,
  openAppIconById,
  openInitGitRepoPrompt,
  handleAddWorkspace,
  openWorkspaceFromUrlPrompt,
  handleAddAgent,
  handleAddWorktreeAgent,
  handleAddCloneAgent,
  handleOpenThreadLink,
  handleSelectOpenAppId,
  handleCopyThread,
  handleToggleTerminalWithFocus,
  launchScriptState,
  launchScriptsState,
  models,
  selectedModelId,
  onSelectModel,
  reasoningOptions,
  selectedEffort,
  onSelectEffort,
  selectedServiceTier,
  reasoningSupported,
  skills,
  prompts,
  composerInputRef,
  composerEditorSettings,
  composerEditorExpanded,
  onToggleComposerEditorExpanded,
  dictationReady,
  dictationState,
  dictationLevel,
  onToggleDictation,
  onCancelDictation,
  clearDictationTranscript,
  clearDictationError,
  clearDictationHint,
  handleComposerSendWithDraftStart,
  interruptTurn,
  terminalOpen,
  debugOpen,
  debugEntries,
  terminalTabs,
  activeTerminalId,
  onSelectTerminal,
  onNewTerminal,
  onCloseTerminal,
  terminalState,
  onClearDebug,
  onCopyDebug,
  onResizeDebug,
  onResizeTerminal,
  isCompact,
  isPhone,
  activeTab,
  setActiveTab,
  tabletTab,
  showMobilePollingFetchStatus,
  appModalsAboutOpen,
  updaterState,
  startUpdate,
  dismissUpdate,
  postUpdateNotice,
  dismissPostUpdateNotice,
  errorToasts,
  dismissErrorToast,
  showDebugButton,
  handleDebugClick,
}: UseMainAppLayoutSurfacesArgs): LayoutNodesOptions {
  const context: MainAppLayoutSurfacesContext = {
    appSettings,
    workspaces,
    groupedWorkspaces,
    workspaceGroupsCount,
    deletingWorktreeIds,
    newAgentDraftWorkspaceId,
    startingDraftThreadWorkspaceId,
    threadsByWorkspace,
    threadParentById,
    threadStatusById,
    threadResumeLoadingById,
    threadListLoadingByWorkspace,
    threadListPagingByWorkspace,
    threadListCursorByWorkspace,
    pinnedThreadsVersion,
    threadListSortKey,
    onSetThreadListSortKey,
    threadListOrganizeMode,
    onSetThreadListOrganizeMode,
    onRefreshAllThreads,
    activeWorkspace,
    activeWorkspaceId,
    activeThreadId,
    activeThreadReadOnly,
    activeItems,
    onPlanAccept,
    onPlanSubmitChanges,
    activePlan,
    activeTokenUsage,
    latestAgentRuns,
    isLoadingLatestAgents,
    gitState,
    composerWorkspaceState,
    promptActions,
    worktreeState,
    sidebarHandlers,
    displayNodes,
    threadPinning,
    workspaceDrop,
    threadNavigation,
    pullRequestComposer,
    dictationUi,
    openAppIconById,
    openInitGitRepoPrompt,
    handleAddWorkspace,
    openWorkspaceFromUrlPrompt,
    handleAddAgent,
    handleAddWorktreeAgent,
    handleAddCloneAgent,
    handleOpenThreadLink,
    handleSelectOpenAppId,
    handleCopyThread,
    handleToggleTerminalWithFocus,
    launchScriptState,
    launchScriptsState,
    models,
    selectedModelId,
    onSelectModel,
    reasoningOptions,
    selectedEffort,
    onSelectEffort,
    selectedServiceTier,
    reasoningSupported,
    skills,
    prompts,
    composerInputRef,
    composerEditorSettings,
    composerEditorExpanded,
    onToggleComposerEditorExpanded,
    dictationReady,
    dictationState,
    dictationLevel,
    onToggleDictation,
    onCancelDictation,
    clearDictationTranscript,
    clearDictationError,
    clearDictationHint,
    handleComposerSendWithDraftStart,
    interruptTurn,
    terminalOpen,
    debugOpen,
    debugEntries,
    terminalTabs,
    activeTerminalId,
    onSelectTerminal,
    onNewTerminal,
    onCloseTerminal,
    terminalState,
    onClearDebug,
    onCopyDebug,
    onResizeDebug,
    onResizeTerminal,
    isCompact,
    isPhone,
    activeTab,
    setActiveTab,
    tabletTab,
    showMobilePollingFetchStatus,
    appModalsAboutOpen,
    updaterState,
    startUpdate,
    dismissUpdate,
    postUpdateNotice,
    dismissPostUpdateNotice,
    errorToasts,
    dismissErrorToast,
    showDebugButton,
    handleDebugClick,
  };

  return {
    primary: buildPrimarySurface(context),
    git: buildGitSurface(context),
    secondary: buildSecondarySurface(context),
  };
}

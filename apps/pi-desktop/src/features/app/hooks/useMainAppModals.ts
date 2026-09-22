import type { GitWorkspace, GitRequest } from "../../git/gitContext";
import { useCallback, useMemo, useRef } from "react";
import type { ComponentType } from "react";
import type {
  AppSettings,
  BranchInfo,
  ThreadSummary,
  WorkspaceGroup,
  WorkspaceInfo,
  WorkspaceSettings,
} from "@/types";
import { useSettingsModalState } from "@app/hooks/useSettingsModalState";
import type { SettingsSection } from "@app/hooks/useSettingsModalState";
import type { AppModalsProps } from "@app/components/AppModals";
import type { SettingsViewProps } from "@settings/components/SettingsView";
import { useRenameThreadPrompt } from "@threads/hooks/useRenameThreadPrompt";
import { useBranchSwitcher } from "@/features/git/hooks/useBranchSwitcher";
import { useInitGitRepoPrompt } from "@/features/git/hooks/useInitGitRepoPrompt";
import type { InitGitRepoOutcome } from "@/features/git/hooks/useGitActions";
import { useWorktreePrompt } from "@/features/workspaces/hooks/useWorktreePrompt";
import { useClonePrompt } from "@/features/workspaces/hooks/useClonePrompt";

type GroupedWorkspaceInfo = SettingsViewProps["groupedWorkspaces"];

type UseMainAppModalsArgs = {
  settingsViewComponent: ComponentType<SettingsViewProps>;
  workspaces: WorkspaceInfo[];
  workspaceGroups: WorkspaceGroup[];
  groupedWorkspaces: GroupedWorkspaceInfo;
  ungroupedLabel: string;
  activeWorkspace: WorkspaceInfo | null;
  setActiveWorkspaceId: (id: string) => void;
  branches: BranchInfo[];
  currentBranch: string | null;
  threadRename: {
    threadsByWorkspace: Record<string, ThreadSummary[]>;
    renameThread: (workspaceId: string, threadId: string, name: string) => void;
  };
  git: {
    workspace: GitWorkspace | null;
    worktreeDelivery: AppModalsProps["worktreeDelivery"];
    checkoutBranch: (name: string) => Promise<void>;
    initGitRepo: (branch: string) => Promise<InitGitRepoOutcome>;
    createGitHubRepo: (
      repo: string,
      visibility: "private" | "public",
      branch: string,
      initializedRequest?: GitRequest,
    ) => Promise<{ ok: true } | { ok: false; error: string }>;
    refreshGitRemote: () => void;
    initGitRepoLoading: boolean;
    createGitHubRepoLoading: boolean;
  };
  workspacePrompts: {
    workspaceProjectPrompt: AppModalsProps["workspaceProjectPrompt"];
    addWorktreeAgent: (
      workspace: WorkspaceInfo,
      branch: string,
      options: { displayName?: string | null; copyAgentsMd?: boolean; activate?: boolean; planId: string },
    ) => Promise<WorkspaceInfo | null>;
    addCloneAgent: (
      workspace: WorkspaceInfo,
      copyName: string,
      copiesFolder: string,
    ) => Promise<WorkspaceInfo | null>;
    updateWorkspaceSettings: (
      id: string,
      settings: Partial<WorkspaceSettings>,
    ) => Promise<WorkspaceInfo>;
    selectWorkspace: (workspaceId: string) => void;
    handleWorktreeCreated: (worktree: WorkspaceInfo, parent: WorkspaceInfo) => Promise<void>;
    resolveCloneProjectContext: (
      workspace: WorkspaceInfo,
    ) => { groupId: string | null; copiesFolder: string | null };
    persistProjectCopiesFolder: (groupId: string, copiesFolder: string) => Promise<void>;
    onCompactActivate?: () => void;
    onWorkspacePromptError: (message: string, kind: "worktree" | "clone") => void;
    openWorkspaceFromUrlPrompt: () => void;
    workspaceFromUrl: Pick<
      AppModalsProps,
      | "workspaceFromUrlPrompt"
      | "workspaceFromUrlCanSubmit"
      | "onWorkspaceFromUrlPromptUrlChange"
      | "onWorkspaceFromUrlPromptTargetFolderNameChange"
      | "onWorkspaceFromUrlPromptChooseDestinationPath"
      | "onWorkspaceFromUrlPromptClearDestinationPath"
      | "onWorkspaceFromUrlPromptCancel"
      | "onWorkspaceFromUrlPromptConfirm"
    >;
  };
  settings: {
    pluginSession?: SettingsViewProps["pluginSession"];
    handleMoveWorkspace: (id: string, direction: "up" | "down") => void;
    removeWorkspace: (workspaceId: string) => Promise<void>;
    createWorkspaceGroup: (name: string) => Promise<WorkspaceGroup | null>;
    renameWorkspaceGroup: (id: string, name: string) => Promise<boolean | null>;
    moveWorkspaceGroup: (id: string, direction: "up" | "down") => Promise<boolean | null>;
    deleteWorkspaceGroup: (id: string) => Promise<boolean | null>;
    assignWorkspaceGroup: (
      workspaceId: string,
      groupId: string | null,
    ) => Promise<boolean | null>;
    reduceTransparency: boolean;
    setReduceTransparency: (value: boolean) => void;
    appSettings: AppSettings;
    openAppIconById: Record<string, string>;
    queueSaveSettings: (next: AppSettings) => Promise<unknown>;
    handleToggleAutomaticAppUpdateChecks: () => void;
    updateWorkspaceSettings: (
      id: string,
      settings: Partial<WorkspaceSettings>,
    ) => Promise<WorkspaceInfo>;
    scaleShortcutTitle: string;
    scaleShortcutText: string;
    handleTestNotificationSound: () => void;
    handleTestSystemNotification: () => void;
    dictationModel: {
      status?: SettingsViewProps["dictationModelStatus"];
      download?: () => void;
      cancel?: () => void;
      remove?: () => void;
    };
  };
};

type UseMainAppModalsResult = {
  appModalsProps: AppModalsProps;
  modalActions: {
    openSettings: (section?: SettingsSection) => void;
    closeSettings: () => void;
    openRenamePrompt: (workspaceId: string, threadId: string) => void;
    openInitGitRepoPrompt: () => void;
    openWorktreePrompt: (workspace: WorkspaceInfo) => void;
    requestWorktree: ReturnType<typeof useWorktreePrompt>["requestWorktree"];
    openClonePrompt: (workspace: WorkspaceInfo) => void;
    openWorkspaceFromUrlPrompt: () => void;
    openBranchSwitcher: () => void;
    closeBranchSwitcher: () => void;
  };
};

type BuildSettingsViewPropsArgs = {
  groupedWorkspaces: GroupedWorkspaceInfo;
  workspaceGroups: WorkspaceGroup[];
  ungroupedLabel: string;
  settings: UseMainAppModalsArgs["settings"];
};

function buildSettingsViewProps({
  groupedWorkspaces,
  workspaceGroups,
  ungroupedLabel,
  settings,
}: BuildSettingsViewPropsArgs): Omit<SettingsViewProps, "initialSection" | "onClose"> {
  return {
    workspaceGroups,
    groupedWorkspaces,
    ungroupedLabel,
    pluginSession: settings.pluginSession,
    onMoveWorkspace: settings.handleMoveWorkspace,
    onDeleteWorkspace: (workspaceId) => {
      void settings.removeWorkspace(workspaceId);
    },
    onCreateWorkspaceGroup: settings.createWorkspaceGroup,
    onRenameWorkspaceGroup: settings.renameWorkspaceGroup,
    onMoveWorkspaceGroup: settings.moveWorkspaceGroup,
    onDeleteWorkspaceGroup: settings.deleteWorkspaceGroup,
    onAssignWorkspaceGroup: settings.assignWorkspaceGroup,
    reduceTransparency: settings.reduceTransparency,
    onToggleTransparency: settings.setReduceTransparency,
    appSettings: settings.appSettings,
    openAppIconById: settings.openAppIconById,
    onUpdateAppSettings: async (next) => {
      await Promise.resolve(settings.queueSaveSettings(next));
    },
    onToggleAutomaticAppUpdateChecks:
      settings.handleToggleAutomaticAppUpdateChecks,
    onUpdateWorkspaceSettings: async (id, nextSettings) => {
      await settings.updateWorkspaceSettings(id, nextSettings);
    },
    scaleShortcutTitle: settings.scaleShortcutTitle,
    scaleShortcutText: settings.scaleShortcutText,
    onTestNotificationSound: settings.handleTestNotificationSound,
    onTestSystemNotification: settings.handleTestSystemNotification,
    dictationModelStatus: settings.dictationModel.status,
    onDownloadDictationModel: settings.dictationModel.download,
    onCancelDictationDownload: settings.dictationModel.cancel,
    onRemoveDictationModel: settings.dictationModel.remove,
  };
}

type BuildAppModalsPropsArgs = {
  workspaceProjectPrompt: AppModalsProps["workspaceProjectPrompt"];
  renamePrompt: AppModalsProps["renamePrompt"];
  onRenamePromptChange: (value: string) => void;
  onRenamePromptCancel: () => void;
  onRenamePromptConfirm: () => void;
  initGitRepoPrompt: AppModalsProps["initGitRepoPrompt"];
  initGitRepoPromptBusy: boolean;
  onInitGitRepoPromptBranchChange: (value: string) => void;
  onInitGitRepoPromptCreateRemoteChange: (value: boolean) => void;
  onInitGitRepoPromptRepoNameChange: (value: string) => void;
  onInitGitRepoPromptPrivateChange: (value: boolean) => void;
  onInitGitRepoPromptCancel: () => void;
  onInitGitRepoPromptConfirm: () => void;
  worktreePrompt: AppModalsProps["worktreePrompt"];
  onWorktreePromptNameChange: (value: string) => void;
  onWorktreePromptChange: (value: string) => void;
  onWorktreePromptCopyAgentsMdChange: (value: boolean) => void;
  onWorktreeSetupScriptChange: (value: string) => void;
  onWorktreePromptCancel: () => void;
  onWorktreePromptConfirm: () => void;
  worktreePlanning: AppModalsProps["worktreePlanning"];
  worktreeDelivery: AppModalsProps["worktreeDelivery"];
  clonePrompt: AppModalsProps["clonePrompt"];
  onClonePromptCopyNameChange: (value: string) => void;
  onClonePromptChooseCopiesFolder: () => void;
  onClonePromptUseSuggestedFolder: () => void;
  onClonePromptClearCopiesFolder: () => void;
  onClonePromptCancel: () => void;
  onClonePromptConfirm: () => void;
  workspaceFromUrl: AppModalsProps["workspaceFromUrlPrompt"] extends null
    ? never
    : Pick<
        AppModalsProps,
        | "workspaceFromUrlPrompt"
        | "workspaceFromUrlCanSubmit"
        | "onWorkspaceFromUrlPromptUrlChange"
        | "onWorkspaceFromUrlPromptTargetFolderNameChange"
        | "onWorkspaceFromUrlPromptChooseDestinationPath"
        | "onWorkspaceFromUrlPromptClearDestinationPath"
        | "onWorkspaceFromUrlPromptCancel"
        | "onWorkspaceFromUrlPromptConfirm"
      >;
  branchSwitcher: AppModalsProps["branchSwitcher"];
  branches: BranchInfo[];
  workspaces: WorkspaceInfo[];
  activeWorkspace: WorkspaceInfo | null;
  currentBranch: string | null;
  onBranchSwitcherSelect: (branch: string, worktree: WorkspaceInfo | null) => void;
  onBranchSwitcherCancel: () => void;
  settingsOpen: boolean;
  settingsSection: SettingsViewProps["initialSection"] | null;
  onCloseSettings: () => void;
  settingsViewComponent: ComponentType<SettingsViewProps>;
  settingsViewProps: Omit<SettingsViewProps, "initialSection" | "onClose">;
};

function buildAppModalsProps({
  workspaceProjectPrompt,
  renamePrompt,
  onRenamePromptChange,
  onRenamePromptCancel,
  onRenamePromptConfirm,
  initGitRepoPrompt,
  initGitRepoPromptBusy,
  onInitGitRepoPromptBranchChange,
  onInitGitRepoPromptCreateRemoteChange,
  onInitGitRepoPromptRepoNameChange,
  onInitGitRepoPromptPrivateChange,
  onInitGitRepoPromptCancel,
  onInitGitRepoPromptConfirm,
  worktreePrompt,
  onWorktreePromptNameChange,
  onWorktreePromptChange,
  onWorktreePromptCopyAgentsMdChange,
  onWorktreeSetupScriptChange,
  onWorktreePromptCancel,
  onWorktreePromptConfirm,
  worktreePlanning,
  worktreeDelivery,
  clonePrompt,
  onClonePromptCopyNameChange,
  onClonePromptChooseCopiesFolder,
  onClonePromptUseSuggestedFolder,
  onClonePromptClearCopiesFolder,
  onClonePromptCancel,
  onClonePromptConfirm,
  workspaceFromUrl,
  branchSwitcher,
  branches,
  workspaces,
  activeWorkspace,
  currentBranch,
  onBranchSwitcherSelect,
  onBranchSwitcherCancel,
  settingsOpen,
  settingsSection,
  onCloseSettings,
  settingsViewComponent,
  settingsViewProps,
}: BuildAppModalsPropsArgs): AppModalsProps {
  return {
    workspaceProjectPrompt,
    renamePrompt,
    onRenamePromptChange,
    onRenamePromptCancel,
    onRenamePromptConfirm,
    initGitRepoPrompt,
    initGitRepoPromptBusy,
    onInitGitRepoPromptBranchChange,
    onInitGitRepoPromptCreateRemoteChange,
    onInitGitRepoPromptRepoNameChange,
    onInitGitRepoPromptPrivateChange,
    onInitGitRepoPromptCancel,
    onInitGitRepoPromptConfirm,
    worktreePrompt,
    onWorktreePromptNameChange,
    onWorktreePromptChange,
    onWorktreePromptCopyAgentsMdChange,
    onWorktreeSetupScriptChange,
    onWorktreePromptCancel,
    onWorktreePromptConfirm,
    worktreePlanning,
    worktreeDelivery,
    clonePrompt,
    onClonePromptCopyNameChange,
    onClonePromptChooseCopiesFolder,
    onClonePromptUseSuggestedFolder,
    onClonePromptClearCopiesFolder,
    onClonePromptCancel,
    onClonePromptConfirm,
    ...workspaceFromUrl,
    branchSwitcher,
    branches,
    workspaces,
    activeWorkspace,
    currentBranch,
    onBranchSwitcherSelect,
    onBranchSwitcherCancel,
    settingsOpen,
    settingsSection: settingsSection ?? undefined,
    onCloseSettings,
    SettingsViewComponent: settingsViewComponent,
    settingsProps: settingsViewProps,
  };
}

export function useMainAppModals({
  settingsViewComponent,
  workspaces,
  workspaceGroups,
  groupedWorkspaces,
  ungroupedLabel,
  activeWorkspace,
  setActiveWorkspaceId,
  branches,
  currentBranch,
  threadRename,
  git,
  workspacePrompts,
  settings,
}: UseMainAppModalsArgs): UseMainAppModalsResult {
  const {
    settingsOpen,
    settingsSection,
    openSettings,
    closeSettings,
  } = useSettingsModalState();

  const {
    renamePrompt,
    openRenamePrompt,
    handleRenamePromptChange,
    handleRenamePromptCancel,
    handleRenamePromptConfirm,
  } = useRenameThreadPrompt({
    threadsByWorkspace: threadRename.threadsByWorkspace,
    renameThread: threadRename.renameThread,
  });

  const {
    branchSwitcher,
    openBranchSwitcher,
    closeBranchSwitcher,
    handleBranchSelect,
  } = useBranchSwitcher({
    activeWorkspace,
    checkoutBranch: git.checkoutBranch,
    setActiveWorkspaceId,
  });

  const {
    initGitRepoPrompt,
    isSubmitting: initGitRepoSubmitting,
    openInitGitRepoPrompt,
    handleInitGitRepoPromptBranchChange,
    handleInitGitRepoPromptCreateRemoteChange,
    handleInitGitRepoPromptRepoNameChange,
    handleInitGitRepoPromptPrivateChange,
    handleInitGitRepoPromptCancel,
    handleInitGitRepoPromptConfirm,
  } = useInitGitRepoPrompt({
    activeWorkspace: git.workspace,
    initGitRepo: git.initGitRepo,
    createGitHubRepo: git.createGitHubRepo,
    refreshGitRemote: git.refreshGitRemote,
    isBusy: git.initGitRepoLoading || git.createGitHubRepoLoading,
  });

  const {
    worktreePrompt,
    openPrompt: openWorktreePromptForWorkspace,
    requestWorktree,
    confirmPrompt: confirmWorktreePrompt,
    reviewPrompt: reviewWorktreePrompt,
    updateCheckout: updateWorktreeCheckout,
    updateExecutionRoot: updateWorktreeExecutionRoot,
    cancelPrompt: cancelWorktreePrompt,
    updateName: updateWorktreeName,
    updateBranch: updateWorktreeBranch,
    updateCopyAgentsMd: updateWorktreeCopyAgentsMd,
    updateSetupScript: updateWorktreeSetupScript,
  } = useWorktreePrompt({
    addWorktreeAgent: workspacePrompts.addWorktreeAgent,
    updateWorkspaceSettings: workspacePrompts.updateWorkspaceSettings,
    onSelectWorkspace: workspacePrompts.selectWorkspace,
    onWorktreeCreated: workspacePrompts.handleWorktreeCreated,
    onCompactActivate: workspacePrompts.onCompactActivate,
    onError: (message) => workspacePrompts.onWorkspacePromptError(message, "worktree"),
  });
  const currentWorkspaces = useRef(workspaces);
  currentWorkspaces.current = workspaces;
  const requestWorktreeForRun = useCallback((workspace: WorkspaceInfo, branch: string) => {
    return requestWorktree(currentWorkspaces.current.find((entry) => entry.id === workspace.id) ?? workspace, branch);
  }, [requestWorktree]);
  const openWorktreePrompt = useCallback((workspace: WorkspaceInfo) => {
    openWorktreePromptForWorkspace(workspace, workspace.id === git.workspace?.id
      ? git.workspace.gitRequest?.threadId ?? null : null);
  }, [git.workspace, openWorktreePromptForWorkspace]);

  const {
    clonePrompt,
    openPrompt: openClonePrompt,
    confirmPrompt: confirmClonePrompt,
    cancelPrompt: cancelClonePrompt,
    updateCopyName: updateCloneCopyName,
    chooseCopiesFolder: chooseCloneCopiesFolder,
    useSuggestedCopiesFolder: useSuggestedCloneCopiesFolder,
    clearCopiesFolder: clearCloneCopiesFolder,
  } = useClonePrompt({
    addCloneAgent: workspacePrompts.addCloneAgent,
    onSelectWorkspace: workspacePrompts.selectWorkspace,
    resolveProjectContext: workspacePrompts.resolveCloneProjectContext,
    persistProjectCopiesFolder: workspacePrompts.persistProjectCopiesFolder,
    onCompactActivate: workspacePrompts.onCompactActivate,
    onError: (message) => workspacePrompts.onWorkspacePromptError(message, "clone"),
  });

  const settingsViewProps = useMemo<Omit<SettingsViewProps, "initialSection" | "onClose">>(
    () =>
      buildSettingsViewProps({
        groupedWorkspaces,
        workspaceGroups,
        ungroupedLabel,
        settings,
      }),
    [groupedWorkspaces, settings, ungroupedLabel, workspaceGroups],
  );

  const appModalsProps = useMemo<AppModalsProps>(
    () =>
      buildAppModalsProps({
        workspaceProjectPrompt: workspacePrompts.workspaceProjectPrompt,
        renamePrompt,
        onRenamePromptChange: handleRenamePromptChange,
        onRenamePromptCancel: handleRenamePromptCancel,
        onRenamePromptConfirm: handleRenamePromptConfirm,
        initGitRepoPrompt,
        initGitRepoPromptBusy: initGitRepoSubmitting || git.initGitRepoLoading || git.createGitHubRepoLoading,
        onInitGitRepoPromptBranchChange: handleInitGitRepoPromptBranchChange,
        onInitGitRepoPromptCreateRemoteChange:
          handleInitGitRepoPromptCreateRemoteChange,
        onInitGitRepoPromptRepoNameChange: handleInitGitRepoPromptRepoNameChange,
        onInitGitRepoPromptPrivateChange: handleInitGitRepoPromptPrivateChange,
        onInitGitRepoPromptCancel: handleInitGitRepoPromptCancel,
        onInitGitRepoPromptConfirm: handleInitGitRepoPromptConfirm,
        worktreePrompt,
        onWorktreePromptNameChange: updateWorktreeName,
        onWorktreePromptChange: updateWorktreeBranch,
        onWorktreePromptCopyAgentsMdChange: updateWorktreeCopyAgentsMd,
        onWorktreeSetupScriptChange: updateWorktreeSetupScript,
        onWorktreePromptCancel: cancelWorktreePrompt,
        onWorktreePromptConfirm: confirmWorktreePrompt,
        worktreeDelivery: git.worktreeDelivery,
        worktreePlanning: {
          reviewPrompt: reviewWorktreePrompt,
          updateCheckout: updateWorktreeCheckout,
          updateExecutionRoot: updateWorktreeExecutionRoot,
        },
        clonePrompt,
        onClonePromptCopyNameChange: updateCloneCopyName,
        onClonePromptChooseCopiesFolder: chooseCloneCopiesFolder,
        onClonePromptUseSuggestedFolder: useSuggestedCloneCopiesFolder,
        onClonePromptClearCopiesFolder: clearCloneCopiesFolder,
        onClonePromptCancel: cancelClonePrompt,
        onClonePromptConfirm: confirmClonePrompt,
        workspaceFromUrl: workspacePrompts.workspaceFromUrl,
        branchSwitcher,
        branches,
        workspaces,
        activeWorkspace,
        currentBranch,
        onBranchSwitcherSelect: handleBranchSelect,
        onBranchSwitcherCancel: closeBranchSwitcher,
        settingsOpen,
        settingsSection,
        onCloseSettings: closeSettings,
        settingsViewComponent,
        settingsViewProps,
      }),
    [
      activeWorkspace,
      branchSwitcher,
      branches,
      cancelClonePrompt,
      cancelWorktreePrompt,
      chooseCloneCopiesFolder,
      clearCloneCopiesFolder,
      clonePrompt,
      closeBranchSwitcher,
      closeSettings,
      confirmClonePrompt,
      confirmWorktreePrompt,
      reviewWorktreePrompt,
      updateWorktreeCheckout,
      updateWorktreeExecutionRoot,
      currentBranch,
      git.worktreeDelivery,
      git.createGitHubRepoLoading,
      git.initGitRepoLoading,
      initGitRepoSubmitting,
      handleBranchSelect,
      handleInitGitRepoPromptBranchChange,
      handleInitGitRepoPromptCancel,
      handleInitGitRepoPromptConfirm,
      handleInitGitRepoPromptCreateRemoteChange,
      handleInitGitRepoPromptPrivateChange,
      handleInitGitRepoPromptRepoNameChange,
      handleRenamePromptCancel,
      handleRenamePromptChange,
      handleRenamePromptConfirm,
      initGitRepoPrompt,
      renamePrompt,
      settingsOpen,
      settingsSection,
      settingsViewComponent,
      settingsViewProps,
      updateCloneCopyName,
      workspacePrompts,
      updateWorktreeBranch,
      updateWorktreeCopyAgentsMd,
      updateWorktreeName,
      updateWorktreeSetupScript,
      useSuggestedCloneCopiesFolder,
      workspaces,
      worktreePrompt,
    ],
  );

  return {
    appModalsProps,
    modalActions: {
      openSettings,
      closeSettings,
      openRenamePrompt,
      openInitGitRepoPrompt,
      openWorktreePrompt,
      requestWorktree: requestWorktreeForRun,
      openClonePrompt,
      openWorkspaceFromUrlPrompt: workspacePrompts.openWorkspaceFromUrlPrompt,
      openBranchSwitcher,
      closeBranchSwitcher,
    },
  };
}

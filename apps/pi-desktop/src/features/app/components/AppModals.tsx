import { lazy, memo, Suspense } from "react";
import type { ComponentType } from "react";
import type { BranchInfo, WorkspaceInfo } from "../../../types";
import type { SettingsViewProps } from "../../settings/components/SettingsView";
import { useRenameThreadPrompt } from "../../threads/hooks/useRenameThreadPrompt";
import { useClonePrompt } from "../../workspaces/hooks/useClonePrompt";
import type { useWorktreeDelivery } from "../../workspaces/hooks/useWorktreeDelivery";
import { useWorktreePrompt } from "../../workspaces/hooks/useWorktreePrompt";
import { useWorkspaceFromUrlPrompt } from "../../workspaces/hooks/useWorkspaceFromUrlPrompt";
import type { BranchSwitcherState } from "../../git/hooks/useBranchSwitcher";
import type { useWorkspaceProjectPrompt } from "../../workspaces/hooks/useWorkspaceProjectPrompt";

const CreateWorkspacePrompt = lazy(() =>
  import("../../workspaces/components/CreateWorkspacePrompt").then((module) => ({
    default: module.CreateWorkspacePrompt,
  })),
);

const RenameThreadPrompt = lazy(() =>
  import("../../threads/components/RenameThreadPrompt").then((module) => ({
    default: module.RenameThreadPrompt,
  })),
);
const WorktreeDeliveryDialog = lazy(() =>
  import("../../workspaces/components/WorktreeDeliveryDialog").then((module) => ({ default: module.WorktreeDeliveryDialog })),
);
const WorktreePrompt = lazy(() =>
  import("../../workspaces/components/WorktreePrompt").then((module) => ({
    default: module.WorktreePrompt,
  })),
);
const ClonePrompt = lazy(() =>
  import("../../workspaces/components/ClonePrompt").then((module) => ({
    default: module.ClonePrompt,
  })),
);
const WorkspaceFromUrlPrompt = lazy(() =>
  import("../../workspaces/components/WorkspaceFromUrlPrompt").then((module) => ({
    default: module.WorkspaceFromUrlPrompt,
  })),
);
const BranchSwitcherPrompt = lazy(() =>
  import("../../git/components/BranchSwitcherPrompt").then((module) => ({
    default: module.BranchSwitcherPrompt,
  })),
);
const InitGitRepoPrompt = lazy(() =>
  import("../../git/components/InitGitRepoPrompt").then((module) => ({
    default: module.InitGitRepoPrompt,
  })),
);

type RenamePromptState = ReturnType<typeof useRenameThreadPrompt>["renamePrompt"];

type WorktreePromptState = ReturnType<typeof useWorktreePrompt>["worktreePrompt"];

type ClonePromptState = ReturnType<typeof useClonePrompt>["clonePrompt"];
type WorkspaceFromUrlPromptState = ReturnType<
  typeof useWorkspaceFromUrlPrompt
>["workspaceFromUrlPrompt"];

export type AppModalsProps = {
  workspaceProjectPrompt: ReturnType<typeof useWorkspaceProjectPrompt>;
  renamePrompt: RenamePromptState;
  onRenamePromptChange: (value: string) => void;
  onRenamePromptCancel: () => void;
  onRenamePromptConfirm: () => void;
  initGitRepoPrompt: {
    workspaceName: string;
    branch: string;
    createRemote: boolean;
    repoName: string;
    isPrivate: boolean;
    error: string | null;
  } | null;
  initGitRepoPromptBusy: boolean;
  onInitGitRepoPromptBranchChange: (value: string) => void;
  onInitGitRepoPromptCreateRemoteChange: (value: boolean) => void;
  onInitGitRepoPromptRepoNameChange: (value: string) => void;
  onInitGitRepoPromptPrivateChange: (value: boolean) => void;
  onInitGitRepoPromptCancel: () => void;
  onInitGitRepoPromptConfirm: () => void;
  worktreeDelivery: ReturnType<typeof useWorktreeDelivery>;
  worktreePrompt: WorktreePromptState;
  onWorktreePromptNameChange: (value: string) => void;
  onWorktreePromptChange: (value: string) => void;
  onWorktreePromptCopyAgentsMdChange: (value: boolean) => void;
  onWorktreeSetupScriptChange: (value: string) => void;
  onWorktreePromptCancel: () => void;
  onWorktreePromptConfirm: () => void;
  worktreePlanning: Pick<ReturnType<typeof useWorktreePrompt>, "reviewPrompt" | "updateCheckout" | "updateExecutionRoot">;
  clonePrompt: ClonePromptState;
  onClonePromptCopyNameChange: (value: string) => void;
  onClonePromptChooseCopiesFolder: () => void;
  onClonePromptUseSuggestedFolder: () => void;
  onClonePromptClearCopiesFolder: () => void;
  onClonePromptCancel: () => void;
  onClonePromptConfirm: () => void;
  workspaceFromUrlPrompt: WorkspaceFromUrlPromptState;
  workspaceFromUrlCanSubmit: boolean;
  onWorkspaceFromUrlPromptUrlChange: (value: string) => void;
  onWorkspaceFromUrlPromptTargetFolderNameChange: (value: string) => void;
  onWorkspaceFromUrlPromptChooseDestinationPath: () => void;
  onWorkspaceFromUrlPromptClearDestinationPath: () => void;
  onWorkspaceFromUrlPromptCancel: () => void;
  onWorkspaceFromUrlPromptConfirm: () => void;
  branchSwitcher: BranchSwitcherState;
  branches: BranchInfo[];
  workspaces: WorkspaceInfo[];
  activeWorkspace: WorkspaceInfo | null;
  currentBranch: string | null;
  onBranchSwitcherSelect: (branch: string, worktree: WorkspaceInfo | null) => void;
  onBranchSwitcherCancel: () => void;
  settingsOpen: boolean;
  settingsSection: SettingsViewProps["initialSection"] | null;
  onCloseSettings: () => void;
  SettingsViewComponent: ComponentType<SettingsViewProps>;
  settingsProps: Omit<SettingsViewProps, "initialSection" | "onClose">;
};

export const AppModals = memo(function AppModals({
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
  worktreeDelivery,
  onWorktreePromptNameChange,
  onWorktreePromptChange,
  onWorktreePromptCopyAgentsMdChange,
  onWorktreeSetupScriptChange,
  onWorktreePromptCancel,
  onWorktreePromptConfirm,
  worktreePlanning,
  clonePrompt,
  onClonePromptCopyNameChange,
  onClonePromptChooseCopiesFolder,
  onClonePromptUseSuggestedFolder,
  onClonePromptClearCopiesFolder,
  onClonePromptCancel,
  onClonePromptConfirm,
  workspaceFromUrlPrompt,
  workspaceFromUrlCanSubmit,
  onWorkspaceFromUrlPromptUrlChange,
  onWorkspaceFromUrlPromptTargetFolderNameChange,
  onWorkspaceFromUrlPromptChooseDestinationPath,
  onWorkspaceFromUrlPromptClearDestinationPath,
  onWorkspaceFromUrlPromptCancel,
  onWorkspaceFromUrlPromptConfirm,
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
  SettingsViewComponent,
  settingsProps,
}: AppModalsProps) {
  return (
    <>
      {workspaceProjectPrompt.prompt && (
        <Suspense fallback={null}>
          <CreateWorkspacePrompt
            {...workspaceProjectPrompt.prompt}
            onNameChange={workspaceProjectPrompt.updateName}
            onChooseDirectories={workspaceProjectPrompt.chooseDirectories}
            onRemoveDirectory={workspaceProjectPrompt.removeDirectory}
            onPrimaryRootChange={workspaceProjectPrompt.updatePrimaryRoot}
            onCancel={workspaceProjectPrompt.cancel}
            onConfirm={workspaceProjectPrompt.confirm}
            onRetryLoad={workspaceProjectPrompt.prompt.mode === "edit" && !workspaceProjectPrompt.prompt.project
              ? workspaceProjectPrompt.retryLoad : undefined}
          />
        </Suspense>
      )}
      {renamePrompt && (
        <Suspense fallback={null}>
          <RenameThreadPrompt
            currentName={renamePrompt.originalName}
            name={renamePrompt.name}
            onChange={onRenamePromptChange}
            onCancel={onRenamePromptCancel}
            onConfirm={onRenamePromptConfirm}
          />
        </Suspense>
      )}
      {initGitRepoPrompt && (
        <Suspense fallback={null}>
          <InitGitRepoPrompt
            workspaceName={initGitRepoPrompt.workspaceName}
            branch={initGitRepoPrompt.branch}
            createRemote={initGitRepoPrompt.createRemote}
            repoName={initGitRepoPrompt.repoName}
            isPrivate={initGitRepoPrompt.isPrivate}
            error={initGitRepoPrompt.error}
            isBusy={initGitRepoPromptBusy}
            onBranchChange={onInitGitRepoPromptBranchChange}
            onCreateRemoteChange={onInitGitRepoPromptCreateRemoteChange}
            onRepoNameChange={onInitGitRepoPromptRepoNameChange}
            onPrivateChange={onInitGitRepoPromptPrivateChange}
            onCancel={onInitGitRepoPromptCancel}
            onConfirm={onInitGitRepoPromptConfirm}
          />
        </Suspense>
      )}
      {worktreeDelivery.state && (
        <Suspense fallback={null}><WorktreeDeliveryDialog delivery={worktreeDelivery} /></Suspense>
      )}
      {worktreePrompt && (
        <Suspense fallback={null}>
          <WorktreePrompt
            workspaceName={worktreePrompt.workspace.name}
            name={worktreePrompt.name}
            branch={worktreePrompt.branch}
            branchWasEdited={worktreePrompt.branchWasEdited}
            copyAgentsMd={worktreePrompt.copyAgentsMd}
            setupScript={worktreePrompt.setupScript}
            scriptError={worktreePrompt.scriptError}
            error={worktreePrompt.error}
            isBusy={worktreePrompt.isSubmitting}
            isSavingScript={worktreePrompt.isSavingScript}
            onNameChange={onWorktreePromptNameChange}
            onChange={onWorktreePromptChange}
            onCopyAgentsMdChange={onWorktreePromptCopyAgentsMdChange}
            onSetupScriptChange={onWorktreeSetupScriptChange}
            onCancel={onWorktreePromptCancel}
            onConfirm={onWorktreePromptConfirm}
            inventory={worktreePrompt.inventory}
            checkouts={worktreePrompt.checkouts}
            executionRootId={worktreePrompt.executionRootId}
            plan={worktreePrompt.plan}
            isLoading={worktreePrompt.isLoading}
            isPreparing={worktreePrompt.isPreparing}
            onReview={worktreePlanning.reviewPrompt}
            onCheckoutChange={worktreePlanning.updateCheckout}
            onExecutionRootChange={worktreePlanning.updateExecutionRoot}
          />
        </Suspense>
      )}
      {clonePrompt && (
        <Suspense fallback={null}>
          <ClonePrompt
            workspaceName={clonePrompt.workspace.name}
            copyName={clonePrompt.copyName}
            copiesFolder={clonePrompt.copiesFolder}
            suggestedCopiesFolder={clonePrompt.suggestedCopiesFolder}
            error={clonePrompt.error}
            isBusy={clonePrompt.isSubmitting}
            onCopyNameChange={onClonePromptCopyNameChange}
            onChooseCopiesFolder={onClonePromptChooseCopiesFolder}
            onUseSuggestedCopiesFolder={onClonePromptUseSuggestedFolder}
            onClearCopiesFolder={onClonePromptClearCopiesFolder}
            onCancel={onClonePromptCancel}
            onConfirm={onClonePromptConfirm}
          />
        </Suspense>
      )}
      {workspaceFromUrlPrompt && (
        <Suspense fallback={null}>
          <WorkspaceFromUrlPrompt
            url={workspaceFromUrlPrompt.url}
            destinationPath={workspaceFromUrlPrompt.destinationPath}
            targetFolderName={workspaceFromUrlPrompt.targetFolderName}
            error={workspaceFromUrlPrompt.error}
            isBusy={workspaceFromUrlPrompt.isSubmitting}
            canSubmit={workspaceFromUrlCanSubmit}
            onUrlChange={onWorkspaceFromUrlPromptUrlChange}
            onTargetFolderNameChange={onWorkspaceFromUrlPromptTargetFolderNameChange}
            onChooseDestinationPath={onWorkspaceFromUrlPromptChooseDestinationPath}
            onClearDestinationPath={onWorkspaceFromUrlPromptClearDestinationPath}
            onCancel={onWorkspaceFromUrlPromptCancel}
            onConfirm={onWorkspaceFromUrlPromptConfirm}
          />
        </Suspense>
      )}
      {branchSwitcher && (
        <Suspense fallback={null}>
          <BranchSwitcherPrompt
            branches={branches}
            workspaces={workspaces}
            activeWorkspace={activeWorkspace}
            currentBranch={currentBranch}
            onSelect={onBranchSwitcherSelect}
            onCancel={onBranchSwitcherCancel}
          />
        </Suspense>
      )}
      {settingsOpen && (
        <Suspense fallback={null}>
          <SettingsViewComponent
            {...settingsProps}
            onClose={onCloseSettings}
            initialSection={settingsSection ?? undefined}
          />
        </Suspense>
      )}
    </>
  );
});

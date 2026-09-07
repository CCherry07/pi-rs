import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import type {
  AppSettings,
  DictationModelStatus,
  WorkspaceGroup,
  WorkspaceSettings,
} from "@/types";
import type { SettingsSection } from "@settings/components/settingsTypes";
import { isMacPlatform, isWindowsPlatform } from "@utils/platformPaths";
import { useSettingsOpenAppDrafts } from "./useSettingsOpenAppDrafts";
import { useSettingsShortcutDrafts } from "./useSettingsShortcutDrafts";
import { useSettingsDisplaySection } from "./useSettingsDisplaySection";
import { useSettingsEnvironmentsSection } from "./useSettingsEnvironmentsSection";
import { useSettingsGitSection } from "./useSettingsGitSection";
import { useSettingsDefaultModels } from "./useSettingsDefaultModels";
import { useSettingsArchivedThreadsSection } from "./useSettingsArchivedThreadsSection";
import { useSettingsProjectsSection } from "./useSettingsProjectsSection";
import type { GroupedWorkspaces } from "./settingsSectionTypes";
import {
  COMPOSER_PRESET_CONFIGS,
  COMPOSER_PRESET_LABEL_KEYS,
  DICTATION_MODELS,
} from "@settings/components/settingsViewConstants";

type UseSettingsViewOrchestrationArgs = {
  activeSection: SettingsSection;
  workspaceGroups: WorkspaceGroup[];
  groupedWorkspaces: GroupedWorkspaces;
  ungroupedLabel: string;
  reduceTransparency: boolean;
  onToggleTransparency: (value: boolean) => void;
  appSettings: AppSettings;
  openAppIconById: Record<string, string>;
  onUpdateAppSettings: (next: AppSettings) => Promise<void>;
  onToggleAutomaticAppUpdateChecks?: () => void;
  onUpdateWorkspaceSettings: (
    id: string,
    settings: Partial<WorkspaceSettings>,
  ) => Promise<void>;
  scaleShortcutTitle: string;
  scaleShortcutText: string;
  onTestNotificationSound: () => void;
  onTestSystemNotification: () => void;
  onMoveWorkspace: (id: string, direction: "up" | "down") => void;
  onDeleteWorkspace: (id: string) => void;
  onCreateWorkspaceGroup: (name: string) => Promise<WorkspaceGroup | null>;
  onRenameWorkspaceGroup: (id: string, name: string) => Promise<boolean | null>;
  onMoveWorkspaceGroup: (id: string, direction: "up" | "down") => Promise<boolean | null>;
  onDeleteWorkspaceGroup: (id: string) => Promise<boolean | null>;
  onAssignWorkspaceGroup: (
    workspaceId: string,
    groupId: string | null,
  ) => Promise<boolean | null>;
  dictationModelStatus?: DictationModelStatus | null;
  onDownloadDictationModel?: () => void;
  onCancelDictationDownload?: () => void;
  onRemoveDictationModel?: () => void;
};

export function useSettingsViewOrchestration({
  activeSection,
  workspaceGroups,
  groupedWorkspaces,
  ungroupedLabel,
  reduceTransparency,
  onToggleTransparency,
  appSettings,
  openAppIconById,
  onUpdateAppSettings,
  onToggleAutomaticAppUpdateChecks,
  onUpdateWorkspaceSettings,
  scaleShortcutTitle,
  scaleShortcutText,
  onTestNotificationSound,
  onTestSystemNotification,
  onMoveWorkspace,
  onDeleteWorkspace,
  onCreateWorkspaceGroup,
  onRenameWorkspaceGroup,
  onMoveWorkspaceGroup,
  onDeleteWorkspaceGroup,
  onAssignWorkspaceGroup,
  dictationModelStatus,
  onDownloadDictationModel,
  onCancelDictationDownload,
  onRemoveDictationModel,
}: UseSettingsViewOrchestrationArgs) {
  const { t } = useTranslation("settings");
  const projects = useMemo(
    () => groupedWorkspaces.flatMap((group) => group.workspaces),
    [groupedWorkspaces],
  );
  const mainWorkspaces = useMemo(
    () => projects.filter((workspace) => (workspace.kind ?? "main") !== "worktree"),
    [projects],
  );

  const optionKeyLabel = isMacPlatform() ? "Option" : "Alt";
  const metaKeyLabel = isMacPlatform()
    ? "Command"
    : isWindowsPlatform()
      ? "Windows"
      : "Meta";
  const followUpShortcutLabel = isMacPlatform()
    ? "Shift+Cmd+Enter"
    : "Shift+Ctrl+Enter";

  const dictationModels = useMemo(() => DICTATION_MODELS.map((model) => ({
    id: model.id,
    size: model.size,
    label: t(model.labelKey as "dictation.models.tiny.label"),
    note: t(model.noteKey as "dictation.models.tiny.note"),
  })), [t]);

  const composerPresetLabels = useMemo(
    () => Object.fromEntries(
      Object.entries(COMPOSER_PRESET_LABEL_KEYS).map(([preset, key]) => [
        preset,
        t(key as "composer.presets.options.default"),
      ]),
    ) as Record<AppSettings["composerEditorPreset"], string>,
    [t],
  );

  const selectedDictationModel = useMemo(() => {
    return (
      dictationModels.find(
        (model) => model.id === appSettings.dictationModelId,
      ) ?? dictationModels[1]
    );
  }, [appSettings.dictationModelId, dictationModels]);

  const dictationReady = dictationModelStatus?.state === "ready";

  const {
    openAppDrafts,
    openAppSelectedId,
    handleOpenAppDraftChange,
    handleOpenAppKindChange,
    handleCommitOpenAppsDrafts,
    handleMoveOpenApp,
    handleDeleteOpenApp,
    handleAddOpenApp,
    handleSelectOpenAppDefault,
  } = useSettingsOpenAppDrafts({
    appSettings,
    onUpdateAppSettings,
  });

  const { shortcutDrafts, handleShortcutKeyDown, clearShortcut } =
    useSettingsShortcutDrafts({
      appSettings,
      onUpdateAppSettings,
    });

  const projectsSectionProps = useSettingsProjectsSection({
    appSettings,
    workspaceGroups,
    groupedWorkspaces,
    ungroupedLabel,
    projects,
    onUpdateAppSettings,
    onMoveWorkspace,
    onDeleteWorkspace,
    onCreateWorkspaceGroup,
    onRenameWorkspaceGroup,
    onMoveWorkspaceGroup,
    onDeleteWorkspaceGroup,
    onAssignWorkspaceGroup,
  });

  const environmentsSectionProps = useSettingsEnvironmentsSection({
    appSettings,
    onUpdateAppSettings,
    mainWorkspaces,
    onUpdateWorkspaceSettings,
  });

  const displaySectionProps = useSettingsDisplaySection({
    appSettings,
    reduceTransparency,
    onToggleTransparency,
    onUpdateAppSettings,
    scaleShortcutTitle,
    scaleShortcutText,
    onTestNotificationSound,
    onTestSystemNotification,
  });

  const defaultModels = useSettingsDefaultModels(projects);

  const gitSectionProps = useSettingsGitSection({
    appSettings,
    onUpdateAppSettings,
    models: defaultModels.models,
  });
  const archivedThreadsSectionProps = useSettingsArchivedThreadsSection({
    enabled: activeSection === "archived",
    projects,
  });

  return {
    aboutSectionProps: {
      appSettings,
      onToggleAutomaticAppUpdateChecks,
    },
    projectsSectionProps,
    environmentsSectionProps,
    displaySectionProps,
    composerSectionProps: {
      appSettings,
      optionKeyLabel,
      followUpShortcutLabel,
      composerPresetLabels,
      onComposerPresetChange: (
        preset: AppSettings["composerEditorPreset"],
      ) => {
        const config = COMPOSER_PRESET_CONFIGS[preset];
        void onUpdateAppSettings({
          ...appSettings,
          composerEditorPreset: preset,
          ...config,
        });
      },
      onUpdateAppSettings,
    },
    dictationSectionProps: {
      appSettings,
      optionKeyLabel,
      metaKeyLabel,
      dictationModels,
      selectedDictationModel,
      dictationModelStatus,
      dictationReady,
      onUpdateAppSettings,
      onDownloadDictationModel,
      onCancelDictationDownload,
      onRemoveDictationModel,
    },
    shortcutsSectionProps: {
      shortcutDrafts,
      onShortcutKeyDown: handleShortcutKeyDown,
      onClearShortcut: clearShortcut,
    },
    openAppsSectionProps: {
      openAppDrafts,
      openAppSelectedId,
      openAppIconById,
      onOpenAppDraftChange: handleOpenAppDraftChange,
      onOpenAppKindChange: handleOpenAppKindChange,
      onCommitOpenApps: handleCommitOpenAppsDrafts,
      onMoveOpenApp: handleMoveOpenApp,
      onDeleteOpenApp: handleDeleteOpenApp,
      onAddOpenApp: handleAddOpenApp,
      onSelectOpenAppDefault: handleSelectOpenAppDefault,
    },
    gitSectionProps,
    archivedThreadsSectionProps,
  };
}

export type SettingsViewOrchestration = ReturnType<typeof useSettingsViewOrchestration>;

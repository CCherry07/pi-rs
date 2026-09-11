import { useCallback, useRef } from "react";
import { ask } from "@tauri-apps/plugin-dialog";
import ChevronLeft from "lucide-react/dist/esm/icons/chevron-left";
import X from "lucide-react/dist/esm/icons/x";
import { useTranslation } from "react-i18next";
import type {
  AppSettings,
  DictationModelStatus,
  WorkspaceSettings,
  WorkspaceGroup,
  WorkspaceInfo,
} from "@/types";
import { useSettingsViewCloseShortcuts } from "@settings/hooks/useSettingsViewCloseShortcuts";
import { useSettingsViewNavigation } from "@settings/hooks/useSettingsViewNavigation";
import { useSettingsViewOrchestration } from "@settings/hooks/useSettingsViewOrchestration";
import { ModalShell } from "@/features/design-system/components/modal/ModalShell";
import { SettingsNav } from "./SettingsNav";
import type { SettingsSection } from "./settingsTypes";
import { SETTINGS_SECTION_LABEL_KEYS } from "./settingsViewConstants";
import { SettingsSectionContainers } from "./sections/SettingsSectionContainers";

import type { PluginSessionContext } from "./sections/SettingsPluginsSection";

export type SettingsViewProps = {
  pluginSession?: PluginSessionContext;
  workspaceGroups: WorkspaceGroup[];
  groupedWorkspaces: Array<{
    id: string | null;
    name: string;
    workspaces: WorkspaceInfo[];
  }>;
  ungroupedLabel: string;
  onClose: () => void;
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
  dictationModelStatus?: DictationModelStatus | null;
  onDownloadDictationModel?: () => void;
  onCancelDictationDownload?: () => void;
  onRemoveDictationModel?: () => void;
  initialSection?: SettingsSection;
};

export function SettingsView({
  workspaceGroups,
  groupedWorkspaces,
  ungroupedLabel,
  onClose,
  onMoveWorkspace,
  onDeleteWorkspace,
  onCreateWorkspaceGroup,
  onRenameWorkspaceGroup,
  onMoveWorkspaceGroup,
  onDeleteWorkspaceGroup,
  onAssignWorkspaceGroup,
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
  dictationModelStatus,
  onDownloadDictationModel,
  onCancelDictationDownload,
  onRemoveDictationModel,
  initialSection,
  pluginSession,
}: SettingsViewProps) {
  const { t } = useTranslation("settings");
  const {
    activeSection,
    showMobileDetail,
    setShowMobileDetail,
    useMobileMasterDetail,
    handleSelectSection,
  } = useSettingsViewNavigation({ initialSection });

  const orchestration = useSettingsViewOrchestration({
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
  });

  const resourceDirty = useRef(false);
  const resourceBusy = useRef(false);
  const onResourceBusyChange = useCallback((busy: boolean) => { resourceBusy.current = busy; }, []);
  const confirming = useRef(false);
  const onResourceDirtyChange = useCallback((dirty: boolean) => { resourceDirty.current = dirty; }, []);
  const leaveResource = useCallback(async (next: () => void) => {
    if (confirming.current || resourceBusy.current) return;
    confirming.current = true;
    try {
      if (!resourceDirty.current || await ask(t("shell.discard"), { title: t("shell.title"), kind: "warning" })) {
        resourceDirty.current = false;
        next();
      }
    } finally { confirming.current = false; }
  }, [t]);
  const closeSettings = useCallback(() => { void leaveResource(onClose); }, [leaveResource, onClose]);
  useSettingsViewCloseShortcuts(closeSettings);

  const activeSectionLabel = t(SETTINGS_SECTION_LABEL_KEYS[activeSection]);
  const settingsBodyClassName = `settings-body${
    useMobileMasterDetail ? " settings-body-mobile-master-detail" : ""
  }${useMobileMasterDetail && showMobileDetail ? " is-detail-visible" : ""}`;

  return (
    <ModalShell
      className="settings-overlay"
      cardClassName="settings-window"
      onBackdropClick={closeSettings}
      ariaLabelledBy="settings-modal-title"
    >
      <div className="settings-titlebar">
        <div className="settings-title" id="settings-modal-title">
          {t("shell.title")}
        </div>
        <button
          type="button"
          className="ghost icon-button settings-close"
          onClick={closeSettings}
          aria-label={t("shell.close")}
        >
          <X aria-hidden />
        </button>
      </div>
      <div className={settingsBodyClassName}>
        {(!useMobileMasterDetail || !showMobileDetail) && (
          <div className="settings-master">
            <SettingsNav
              activeSection={activeSection}
              onSelectSection={(section) => { if (section !== activeSection) void leaveResource(() => handleSelectSection(section)); }}
              showDisclosure={useMobileMasterDetail}
            />
          </div>
        )}
        {(!useMobileMasterDetail || showMobileDetail) && (
          <div className="settings-detail">
            {useMobileMasterDetail && (
              <div className="settings-mobile-detail-header">
                <button
                  type="button"
                  className="settings-mobile-back"
                  onClick={() => { void leaveResource(() => setShowMobileDetail(false)); }}
                  aria-label={t("shell.back")}
                >
                  <ChevronLeft aria-hidden />
                  {t("shell.sections")}
                </button>
                <div className="settings-mobile-detail-title">{activeSectionLabel}</div>
              </div>
            )}
            <div className="settings-content">
              <SettingsSectionContainers
                pluginSession={pluginSession}
                activeSection={activeSection}
                orchestration={orchestration}
                onResourceDirtyChange={onResourceDirtyChange}
                onResourceBusyChange={onResourceBusyChange}
              />
            </div>
          </div>
        )}
      </div>
    </ModalShell>
  );
}

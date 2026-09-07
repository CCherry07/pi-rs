import type { Dispatch, SetStateAction } from "react";
import { useTranslation } from "react-i18next";
import { SettingsSection } from "@/features/design-system/components/settings/SettingsPrimitives";
import type { WorkspaceInfo } from "@/types";
import { pushErrorToast } from "@services/toasts";

type SettingsEnvironmentsSectionProps = {
  mainWorkspaces: WorkspaceInfo[];
  environmentWorkspace: WorkspaceInfo | null;
  environmentSaving: boolean;
  environmentError: string | null;
  environmentDraftScript: string;
  environmentSavedScript: string | null;
  environmentDirty: boolean;
  globalWorktreesFolderDraft: string;
  globalWorktreesFolderSaved: string | null;
  globalWorktreesFolderDirty: boolean;
  worktreesFolderDraft: string;
  worktreesFolderSaved: string | null;
  worktreesFolderDirty: boolean;
  onSetEnvironmentWorkspaceId: Dispatch<SetStateAction<string | null>>;
  onSetEnvironmentDraftScript: Dispatch<SetStateAction<string>>;
  onSetGlobalWorktreesFolderDraft: Dispatch<SetStateAction<string>>;
  onSetWorktreesFolderDraft: Dispatch<SetStateAction<string>>;
  onSaveEnvironmentSetup: () => Promise<void>;
};

export function SettingsEnvironmentsSection({
  mainWorkspaces,
  environmentWorkspace,
  environmentSaving,
  environmentError,
  environmentDraftScript,
  environmentSavedScript,
  environmentDirty,
  globalWorktreesFolderDraft,
  globalWorktreesFolderSaved: _globalWorktreesFolderSaved,
  globalWorktreesFolderDirty,
  worktreesFolderDraft,
  worktreesFolderSaved: _worktreesFolderSaved,
  worktreesFolderDirty,
  onSetEnvironmentWorkspaceId,
  onSetEnvironmentDraftScript,
  onSetGlobalWorktreesFolderDraft,
  onSetWorktreesFolderDraft,
  onSaveEnvironmentSetup,
}: SettingsEnvironmentsSectionProps) {
  const { t } = useTranslation("settings");
  const { t: tCommon } = useTranslation("common");
  const hasAnyChanges =
    environmentDirty || globalWorktreesFolderDirty || worktreesFolderDirty;
  const hasProjects = mainWorkspaces.length > 0;

  return (
    <SettingsSection
      title={t("environments.title")}
      subtitle={t("environments.subtitle")}
    >
      <div className="settings-field">
        <label className="settings-field-label" htmlFor="settings-global-worktrees-folder">
          {t("environments.globalRoot.label")}
        </label>
        <div className="settings-help">
          {t("environments.globalRoot.help")}
        </div>
        <div className="settings-field-row">
          <input
            id="settings-global-worktrees-folder"
            type="text"
            className="settings-input"
            value={globalWorktreesFolderDraft}
            onChange={(event) => onSetGlobalWorktreesFolderDraft(event.target.value)}
            placeholder="/path/to/worktrees-root"
            disabled={environmentSaving}
          />
          <button
            type="button"
            className="ghost settings-button-compact"
            onClick={async () => {
              try {
                const { open } = await import("@tauri-apps/plugin-dialog");
                const selected = await open({
                  directory: true,
                  multiple: false,
                  title: t("environments.globalRoot.pickerTitle"),
                });
                if (selected && typeof selected === "string") {
                  onSetGlobalWorktreesFolderDraft(selected);
                }
              } catch (error) {
                pushErrorToast({
                  title: t("environments.folderPickerFailed"),
                  message: error instanceof Error ? error.message : String(error),
                });
              }
            }}
            disabled={environmentSaving}
          >
            {tCommon("actions.browse")}
          </button>
        </div>
        {!hasProjects ? (
          <div className="settings-field-actions">
            <button
              type="button"
              className="ghost settings-button-compact"
              onClick={() => onSetGlobalWorktreesFolderDraft(_globalWorktreesFolderSaved ?? "")}
              disabled={environmentSaving || !globalWorktreesFolderDirty}
            >
              {tCommon("actions.reset")}
            </button>
            <button
              type="button"
              className="primary settings-button-compact"
              onClick={() => {
                void onSaveEnvironmentSetup();
              }}
              disabled={environmentSaving || !globalWorktreesFolderDirty}
            >
              {environmentSaving ? tCommon("saving") : tCommon("actions.save")}
            </button>
          </div>
        ) : null}
        {!hasProjects && environmentError ? (
          <div className="settings-agents-error">{environmentError}</div>
        ) : null}
      </div>

      {!hasProjects ? (
        <div className="settings-empty">{t("projects.projectList.empty")}</div>
      ) : (
        <>
          <div className="settings-field">
            <label className="settings-field-label" htmlFor="settings-environment-project">
              {t("environments.project")}
            </label>
            <select
              id="settings-environment-project"
              className="settings-select"
              value={environmentWorkspace?.id ?? ""}
              onChange={(event) => onSetEnvironmentWorkspaceId(event.target.value)}
              disabled={environmentSaving}
            >
              {mainWorkspaces.map((workspace) => (
                <option key={workspace.id} value={workspace.id}>
                  {workspace.name}
                </option>
              ))}
            </select>
            {environmentWorkspace ? (
              <div className="settings-help">{environmentWorkspace.path}</div>
            ) : null}
          </div>

          <div className="settings-field">
            <div className="settings-field-label">{t("environments.setupScript.label")}</div>
            <div className="settings-help">
              {t("environments.setupScript.help")}
            </div>
            {environmentError ? (
              <div className="settings-agents-error">{environmentError}</div>
            ) : null}
            <textarea
              className="settings-agents-textarea"
              value={environmentDraftScript}
              onChange={(event) => onSetEnvironmentDraftScript(event.target.value)}
              placeholder="pnpm install"
              spellCheck={false}
              disabled={environmentSaving}
            />
            <div className="settings-field-actions">
              <button
                type="button"
                className="ghost settings-button-compact"
                onClick={() => {
                  const clipboard = typeof navigator === "undefined" ? null : navigator.clipboard;
                  if (!clipboard?.writeText) {
                    pushErrorToast({
                      title: t("environments.copyFailed.title"),
                      message: t("environments.copyFailed.unavailable"),
                    });
                    return;
                  }

                  void clipboard.writeText(environmentDraftScript).catch(() => {
                    pushErrorToast({
                      title: t("environments.copyFailed.title"),
                      message: t("environments.copyFailed.write"),
                    });
                  });
                }}
                disabled={environmentSaving || environmentDraftScript.length === 0}
              >
                {tCommon("actions.copy")}
              </button>
              <button
                type="button"
                className="ghost settings-button-compact"
                onClick={() => onSetEnvironmentDraftScript(environmentSavedScript ?? "")}
                disabled={environmentSaving || !environmentDirty}
              >
                {tCommon("actions.reset")}
              </button>
              <button
                type="button"
                className="primary settings-button-compact"
                onClick={() => {
                  void onSaveEnvironmentSetup();
                }}
                disabled={environmentSaving || !hasAnyChanges}
              >
                {environmentSaving ? tCommon("saving") : tCommon("actions.save")}
              </button>
            </div>
          </div>

          <div className="settings-field">
            <label className="settings-field-label" htmlFor="settings-worktrees-folder">
              {t("environments.projectRoot.label")}
            </label>
            <div className="settings-help">
              {t("environments.projectRoot.help")}
            </div>
            <div className="settings-field-row">
              <input
                id="settings-worktrees-folder"
                type="text"
                className="settings-input"
                value={worktreesFolderDraft}
                onChange={(event) => onSetWorktreesFolderDraft(event.target.value)}
                placeholder="/path/to/worktrees"
                disabled={environmentSaving}
              />
              <button
                type="button"
                className="ghost settings-button-compact"
                onClick={async () => {
                  try {
                    const { open } = await import("@tauri-apps/plugin-dialog");
                    const selected = await open({
                      directory: true,
                      multiple: false,
                      title: t("environments.projectRoot.pickerTitle"),
                    });
                    if (selected && typeof selected === "string") {
                      onSetWorktreesFolderDraft(selected);
                    }
                  } catch (error) {
                    pushErrorToast({
                      title: t("environments.folderPickerFailed"),
                      message: error instanceof Error ? error.message : String(error),
                    });
                  }
                }}
                disabled={environmentSaving}
              >
                {tCommon("actions.browse")}
              </button>
            </div>
          </div>
        </>
      )}
    </SettingsSection>
  );
}

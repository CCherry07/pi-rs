import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { AppSettings } from "@/types";
import { getAppBuildType, type AppBuildType } from "@services/tauri";
import { useUpdater } from "@/features/update/hooks/useUpdater";
import {
  SettingsSection,
  SettingsToggleRow,
  SettingsToggleSwitch,
} from "@/features/design-system/components/settings/SettingsPrimitives";
import { formatLocalizedDate, formatLocalizedNumber } from "@/i18n/format";

type SettingsAboutSectionProps = {
  appSettings: AppSettings;
  onToggleAutomaticAppUpdateChecks?: () => void;
};

function formatBytes(value: number) {
  if (!Number.isFinite(value) || value <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB"];
  let size = value;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }
  return `${formatLocalizedNumber(size, {
    maximumFractionDigits: size >= 10 ? 0 : 1,
  })} ${units[unitIndex]}`;
}

export function SettingsAboutSection({
  appSettings,
  onToggleAutomaticAppUpdateChecks,
}: SettingsAboutSectionProps) {
  const { t } = useTranslation("settings");
  const [appBuildType, setAppBuildType] = useState<AppBuildType | "unknown">("unknown");
  const { state: updaterState, checkForUpdates, startUpdate } = useUpdater({
    enabled: true,
    autoCheckOnMount: false,
  });

  useEffect(() => {
    let active = true;
    const loadBuildType = async () => {
      try {
        const value = await getAppBuildType();
        if (active) {
          setAppBuildType(value);
        }
      } catch {
        if (active) {
          setAppBuildType("unknown");
        }
      }
    };
    void loadBuildType();
    return () => {
      active = false;
    };
  }, []);

  const buildDateValue = __APP_BUILD_DATE__.trim();
  const parsedBuildDate = Date.parse(buildDateValue);
  const buildDateLabel = Number.isNaN(parsedBuildDate)
    ? buildDateValue || t("about.unknown")
    : formatLocalizedDate(parsedBuildDate, { dateStyle: "medium", timeStyle: "medium" });
  const buildTypeLabel =
    appBuildType === "debug"
      ? t("about.buildTypes.debug")
      : appBuildType === "release"
        ? t("about.buildTypes.release")
        : t("about.unknown");

  return (
    <SettingsSection title={t("about.title")} subtitle={t("about.subtitle")}>
      <div className="settings-field">
        <div className="settings-help">
          {t("about.version")}: <code>{__APP_VERSION__}</code>
        </div>
        <div className="settings-help">
          {t("about.buildType")}: <code>{buildTypeLabel}</code>
        </div>
        <div className="settings-help">
          {t("about.branch")}: <code>{__APP_GIT_BRANCH__ || t("about.unknown")}</code>
        </div>
        <div className="settings-help">
          {t("about.commit")}: <code>{__APP_COMMIT_HASH__ || t("about.unknown")}</code>
        </div>
        <div className="settings-help">
          {t("about.buildDate")}: <code>{buildDateLabel}</code>
        </div>
      </div>
      <div className="settings-field">
        <div className="settings-label">{t("about.updates.title")}</div>
        <SettingsToggleRow
          title={t("about.updates.automatic.title")}
          subtitle={t("about.updates.automatic.subtitle")}
        >
          <SettingsToggleSwitch
            pressed={appSettings.automaticAppUpdateChecksEnabled}
            onClick={() => {
              onToggleAutomaticAppUpdateChecks?.();
            }}
          />
        </SettingsToggleRow>
        <div className="settings-help">
          {t("about.updates.currentVersion")} <code>{__APP_VERSION__}</code>
        </div>
        {updaterState.stage === "error" && (
          <div className="settings-help ds-text-danger">
            {t("about.updates.failed", { error: updaterState.error })}
          </div>
        )}

        {updaterState.stage === "downloading" ||
        updaterState.stage === "installing" ||
        updaterState.stage === "restarting" ? (
          <div className="settings-help">
            {updaterState.stage === "downloading" ? (
              <>
                {t("about.updates.downloading")} {" "}
                {updaterState.progress?.totalBytes
                  ? `${Math.round((updaterState.progress.downloadedBytes / updaterState.progress.totalBytes) * 100)}%`
                  : formatBytes(updaterState.progress?.downloadedBytes ?? 0)}
              </>
            ) : updaterState.stage === "installing" ? (
              t("about.updates.installing")
            ) : (
              t("about.updates.restarting")
            )}
          </div>
        ) : updaterState.stage === "available" ? (
          <div className="settings-help">
            {t("about.updates.availableBefore")} <code>{updaterState.version}</code>{" "}
            {t("about.updates.availableAfter")}
          </div>
        ) : updaterState.stage === "latest" ? (
          <div className="settings-help">{t("about.updates.latest")}</div>
        ) : null}

        <div className="settings-controls">
          {updaterState.stage === "available" ? (
            <button
              type="button"
              className="primary"
              onClick={() => void startUpdate()}
            >
              {t("about.updates.downloadInstall")}
            </button>
          ) : (
            <button
              type="button"
              className="ghost"
              disabled={
                updaterState.stage === "checking" ||
                updaterState.stage === "downloading" ||
                updaterState.stage === "installing" ||
                updaterState.stage === "restarting"
              }
              onClick={() => void checkForUpdates({ announceNoUpdate: true })}
            >
              {updaterState.stage === "checking"
                ? t("about.updates.checking")
                : t("about.updates.check")}
            </button>
          )}
        </div>
      </div>
    </SettingsSection>
  );
}

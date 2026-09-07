import ArchiveRestore from "lucide-react/dist/esm/icons/archive-restore";
import RefreshCw from "lucide-react/dist/esm/icons/refresh-cw";
import Trash from "lucide-react/dist/esm/icons/trash";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import {
  SettingsSection,
  SettingsSubsection,
} from "@/features/design-system/components/settings/SettingsPrimitives";
import type {
  ArchivedThreadEntry,
  SettingsArchivedThreadsSectionProps,
} from "@settings/hooks/useSettingsArchivedThreadsSection";

const formatUpdatedAt = (
  timestamp: number,
  locale: string,
  fallback: string,
) => {
  if (!Number.isFinite(timestamp) || timestamp <= 0) {
    return fallback;
  }
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(timestamp));
};

const threadKey = (entry: ArchivedThreadEntry) =>
  `${entry.workspaceId}:${entry.id}`;

export function SettingsArchivedThreadsSection({
  archivedThreads,
  loading,
  error,
  busyThreadActions,
  deletingAll,
  onRefresh,
  onRestoreThread,
  onDeleteThread,
  onDeleteAllThreads,
}: SettingsArchivedThreadsSectionProps) {
  const { t, i18n } = useTranslation("settings");
  const { t: tCommon } = useTranslation("common");
  const locale = i18n.language.startsWith("zh") ? "zh-CN" : "en-US";
  const emptyText = loading ? t("archived.loading") : t("archived.empty");
  const hasBusyThread = Object.keys(busyThreadActions).length > 0;
  const rows = useMemo(
    () =>
      archivedThreads.map((entry) => ({
        entry,
        key: threadKey(entry),
        title: entry.name || t("archived.untitled"),
        updatedAt: formatUpdatedAt(
          entry.updatedAt,
          locale,
          t("archived.unknownUpdatedAt"),
        ),
      })),
    [archivedThreads, locale, t],
  );

  return (
    <SettingsSection
      title={t("archived.title")}
      subtitle={t("archived.subtitle")}
    >
      <SettingsSubsection
        title={t("archived.listTitle")}
        subtitle={t("archived.listSubtitle")}
      />
      <div className="settings-field-actions settings-archived-toolbar">
        <button
          type="button"
          className="ghost settings-button-compact"
          onClick={onRefresh}
          disabled={loading || deletingAll || hasBusyThread}
        >
          <RefreshCw aria-hidden />
          {tCommon("actions.refresh")}
        </button>
        <button
          type="button"
          className="ghost danger settings-button-compact"
          onClick={() => {
            void onDeleteAllThreads();
          }}
          disabled={loading || rows.length === 0 || deletingAll || hasBusyThread}
        >
          <Trash aria-hidden strokeWidth={1.8} />
          {deletingAll ? t("archived.deletingAll") : t("archived.deleteAll")}
        </button>
      </div>
      {error ? <div className="settings-group-error">{error}</div> : null}
      {rows.length === 0 ? (
        <div className="settings-empty">{emptyText}</div>
      ) : (
        <div className="settings-archived-list">
          {rows.map(({ entry, key, title, updatedAt }) => {
            const action = busyThreadActions[key];
            const rowBusy = action !== undefined;
            return (
              <div key={key} className="settings-archived-row">
                <div className="settings-archived-info">
                  <div className="settings-archived-title">{title}</div>
                  <div className="settings-archived-meta">
                    <span>{entry.workspaceName}</span>
                    <span>{updatedAt}</span>
                    {typeof entry.messageCount === "number" ? (
                      <span>
                        {t("archived.messageCount", {
                          count: entry.messageCount,
                        })}
                      </span>
                    ) : null}
                  </div>
                  <div className="settings-archived-path">
                    {entry.workspacePath}
                  </div>
                </div>
                <div className="settings-archived-actions">
                  <button
                    type="button"
                    className="ghost settings-button-compact"
                    onClick={() => {
                      void onRestoreThread(entry);
                    }}
                    disabled={deletingAll || rowBusy}
                  >
                    <ArchiveRestore aria-hidden strokeWidth={1.8} />
                    {action === "restore"
                      ? t("archived.restoring")
                      : t("archived.restore")}
                  </button>
                  <button
                    type="button"
                    className="ghost danger settings-button-compact"
                    onClick={() => {
                      void onDeleteThread(entry);
                    }}
                    disabled={deletingAll || rowBusy}
                  >
                    <Trash aria-hidden strokeWidth={1.8} />
                    {action === "delete"
                      ? t("archived.deleting")
                      : tCommon("actions.delete")}
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </SettingsSection>
  );
}

import { useCallback, type Dispatch, type SetStateAction } from "react";
import type { AppSettings } from "@/types";
import { OPEN_APP_STORAGE_KEY } from "@app/constants";

type UseMainAppSettingsActionsArgs = {
  appSettings: AppSettings;
  setAppSettings: Dispatch<SetStateAction<AppSettings>>;
  queueSaveSettings: (next: AppSettings) => Promise<unknown>;
};

export function useMainAppSettingsActions({
  appSettings,
  setAppSettings,
  queueSaveSettings,
}: UseMainAppSettingsActionsArgs) {
  const handleSelectOpenAppId = useCallback(
    (id: string) => {
      if (typeof window !== "undefined") {
        window.localStorage.setItem(OPEN_APP_STORAGE_KEY, id);
      }
      setAppSettings((current) => {
        if (current.selectedOpenAppId === id) {
          return current;
        }
        const nextSettings = {
          ...current,
          selectedOpenAppId: id,
        };
        void queueSaveSettings(nextSettings);
        return nextSettings;
      });
    },
    [queueSaveSettings, setAppSettings],
  );

  const handleToggleAutomaticAppUpdateChecks = useCallback(() => {
    setAppSettings((current) => {
      const nextSettings = {
        ...current,
        automaticAppUpdateChecksEnabled: !current.automaticAppUpdateChecksEnabled,
      };
      void queueSaveSettings(nextSettings);
      return nextSettings;
    });
  }, [queueSaveSettings, setAppSettings]);

  const persistProjectCopiesFolder = useCallback(
    async (groupId: string, copiesFolder: string) => {
      await queueSaveSettings({
        ...appSettings,
        workspaceGroups: appSettings.workspaceGroups.map((entry) =>
          entry.id === groupId ? { ...entry, copiesFolder } : entry,
        ),
      });
    },
    [appSettings, queueSaveSettings],
  );

  return {
    handleSelectOpenAppId,
    handleToggleAutomaticAppUpdateChecks,
    persistProjectCopiesFolder,
  };
}

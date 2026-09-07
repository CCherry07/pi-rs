import type { AppSettings, DictationModelStatus } from "@/types";
import { useTranslation } from "react-i18next";
import {
  SettingsSection,
  SettingsToggleRow,
  SettingsToggleSwitch,
} from "@/features/design-system/components/settings/SettingsPrimitives";
import { formatDownloadSize } from "@utils/formatting";

type DictationModelOption = {
  id: string;
  label: string;
  size: string;
  note: string;
};

type SettingsDictationSectionProps = {
  appSettings: AppSettings;
  optionKeyLabel: string;
  metaKeyLabel: string;
  dictationModels: DictationModelOption[];
  selectedDictationModel: DictationModelOption;
  dictationModelStatus?: DictationModelStatus | null;
  dictationReady: boolean;
  onUpdateAppSettings: (next: AppSettings) => Promise<void>;
  onDownloadDictationModel?: () => void;
  onCancelDictationDownload?: () => void;
  onRemoveDictationModel?: () => void;
};

export function SettingsDictationSection({
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
}: SettingsDictationSectionProps) {
  const { t } = useTranslation("settings");
  const dictationProgress = dictationModelStatus?.progress ?? null;

  return (
    <SettingsSection
      title={t("dictation.title")}
      subtitle={t("dictation.subtitle")}
    >
      <SettingsToggleRow
        title={t("dictation.enable.title")}
        subtitle={t("dictation.enable.subtitle")}
      >
        <SettingsToggleSwitch
          pressed={appSettings.dictationEnabled}
          onClick={() => {
            const nextEnabled = !appSettings.dictationEnabled;
            void onUpdateAppSettings({
              ...appSettings,
              dictationEnabled: nextEnabled,
            });
            if (
              !nextEnabled &&
              dictationModelStatus?.state === "downloading" &&
              onCancelDictationDownload
            ) {
              onCancelDictationDownload();
            }
            if (
              nextEnabled &&
              dictationModelStatus?.state === "missing" &&
              onDownloadDictationModel
            ) {
              onDownloadDictationModel();
            }
          }}
        />
      </SettingsToggleRow>
      <div className="settings-field">
        <label className="settings-field-label" htmlFor="dictation-model">
          {t("dictation.model.label")}
        </label>
        <select
          id="dictation-model"
          className="settings-select"
          value={appSettings.dictationModelId}
          onChange={(event) =>
            void onUpdateAppSettings({
              ...appSettings,
              dictationModelId: event.target.value,
            })
          }
        >
          {dictationModels.map((model) => (
            <option key={model.id} value={model.id}>
              {model.label} ({model.size})
            </option>
          ))}
        </select>
        <div className="settings-help">
          {t("dictation.model.help", {
            note: selectedDictationModel.note,
            size: selectedDictationModel.size,
          })}
        </div>
      </div>
      <div className="settings-field">
        <label className="settings-field-label" htmlFor="dictation-language">
          {t("dictation.language.label")}
        </label>
        <select
          id="dictation-language"
          className="settings-select"
          value={appSettings.dictationPreferredLanguage ?? ""}
          onChange={(event) =>
            void onUpdateAppSettings({
              ...appSettings,
              dictationPreferredLanguage: event.target.value || null,
            })
          }
        >
          <option value="">{t("dictation.language.auto")}</option>
          <option value="en">{t("dictation.language.options.en")}</option>
          <option value="es">{t("dictation.language.options.es")}</option>
          <option value="fr">{t("dictation.language.options.fr")}</option>
          <option value="de">{t("dictation.language.options.de")}</option>
          <option value="it">{t("dictation.language.options.it")}</option>
          <option value="pt">{t("dictation.language.options.pt")}</option>
          <option value="nl">{t("dictation.language.options.nl")}</option>
          <option value="sv">{t("dictation.language.options.sv")}</option>
          <option value="no">{t("dictation.language.options.no")}</option>
          <option value="da">{t("dictation.language.options.da")}</option>
          <option value="fi">{t("dictation.language.options.fi")}</option>
          <option value="pl">{t("dictation.language.options.pl")}</option>
          <option value="tr">{t("dictation.language.options.tr")}</option>
          <option value="ru">{t("dictation.language.options.ru")}</option>
          <option value="uk">{t("dictation.language.options.uk")}</option>
          <option value="ja">{t("dictation.language.options.ja")}</option>
          <option value="ko">{t("dictation.language.options.ko")}</option>
          <option value="zh">{t("dictation.language.options.zh")}</option>
        </select>
        <div className="settings-help">
          {t("dictation.language.help")}
        </div>
      </div>
      <div className="settings-field">
        <label className="settings-field-label" htmlFor="dictation-hold-key">
          {t("dictation.holdKey.label")}
        </label>
        <select
          id="dictation-hold-key"
          className="settings-select"
          value={appSettings.dictationHoldKey ?? ""}
          onChange={(event) =>
            void onUpdateAppSettings({
              ...appSettings,
              dictationHoldKey: event.target.value,
            })
          }
        >
          <option value="">{t("dictation.holdKey.off")}</option>
          <option value="alt">{optionKeyLabel}</option>
          <option value="shift">Shift</option>
          <option value="control">Control</option>
          <option value="meta">{metaKeyLabel}</option>
        </select>
        <div className="settings-help">
          {t("dictation.holdKey.help")}
        </div>
      </div>
      {dictationModelStatus && (
        <div className="settings-field">
          <div className="settings-field-label">
            {t("dictation.status.label", { model: selectedDictationModel.label })}
          </div>
          <div className="settings-help">
            {dictationModelStatus.state === "ready" && t("dictation.status.ready")}
            {dictationModelStatus.state === "missing" && t("dictation.status.missing")}
            {dictationModelStatus.state === "downloading" && t("dictation.status.downloading")}
            {dictationModelStatus.state === "error" &&
              (dictationModelStatus.error ?? t("dictation.status.error"))}
          </div>
          {dictationProgress && (
            <div className="settings-download-progress">
              <div className="settings-download-bar">
                <div
                  className="settings-download-fill"
                  style={{
                    width: dictationProgress.totalBytes
                      ? `${Math.min(
                          100,
                          (dictationProgress.downloadedBytes / dictationProgress.totalBytes) * 100,
                        )}%`
                      : "0%",
                  }}
                />
              </div>
              <div className="settings-download-meta">
                {formatDownloadSize(dictationProgress.downloadedBytes)}
              </div>
            </div>
          )}
          <div className="settings-field-actions">
            {dictationModelStatus.state === "missing" && (
              <button
                type="button"
                className="primary"
                onClick={onDownloadDictationModel}
                disabled={!onDownloadDictationModel}
              >
                {t("dictation.actions.download")}
              </button>
            )}
            {dictationModelStatus.state === "downloading" && (
              <button
                type="button"
                className="ghost settings-button-compact"
                onClick={onCancelDictationDownload}
                disabled={!onCancelDictationDownload}
              >
                {t("dictation.actions.cancel")}
              </button>
            )}
            {dictationReady && (
              <button
                type="button"
                className="ghost settings-button-compact"
                onClick={onRemoveDictationModel}
                disabled={!onRemoveDictationModel}
              >
                {t("dictation.actions.remove")}
              </button>
            )}
          </div>
        </div>
      )}
    </SettingsSection>
  );
}

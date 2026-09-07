import type { AppSettings } from "@/types";
import { useTranslation } from "react-i18next";
import {
  SettingsSection,
  SettingsToggleRow,
  SettingsToggleSwitch,
} from "@/features/design-system/components/settings/SettingsPrimitives";

type ComposerPreset = AppSettings["composerEditorPreset"];

type SettingsComposerSectionProps = {
  appSettings: AppSettings;
  optionKeyLabel: string;
  followUpShortcutLabel: string;
  composerPresetLabels: Record<ComposerPreset, string>;
  onComposerPresetChange: (preset: ComposerPreset) => void;
  onUpdateAppSettings: (next: AppSettings) => Promise<void>;
};

export function SettingsComposerSection({
  appSettings,
  optionKeyLabel,
  followUpShortcutLabel,
  composerPresetLabels,
  onComposerPresetChange,
  onUpdateAppSettings,
}: SettingsComposerSectionProps) {
  const { t } = useTranslation("settings");
  const steerUnavailable = !appSettings.steerEnabled;
  return (
    <SettingsSection
      title={t("composer.title")}
      subtitle={t("composer.subtitle")}
    >
      <div className="settings-field">
        <div className="settings-field-label">{t("composer.followUp.title")}</div>
        <div className={`settings-segmented${appSettings.followUpMessageBehavior === "steer" ? " is-second-active" : ""}`} aria-label={t("composer.followUp.title")}>
          <label
            className={`settings-segmented-option${
              appSettings.followUpMessageBehavior === "queue" ? " is-active" : ""
            }`}
          >
            <input
              className="settings-segmented-input"
              type="radio"
              name="follow-up-behavior"
              value="queue"
              checked={appSettings.followUpMessageBehavior === "queue"}
              onChange={() =>
                void onUpdateAppSettings({
                  ...appSettings,
                  followUpMessageBehavior: "queue",
                })
              }
            />
            <span className="settings-segmented-option-label">{t("composer.followUp.queue")}</span>
          </label>
          <label
            className={`settings-segmented-option${
              appSettings.followUpMessageBehavior === "steer" ? " is-active" : ""
            }${steerUnavailable ? " is-disabled" : ""}`}
            title={steerUnavailable ? t("composer.followUp.steerUnavailable") : ""}
          >
            <input
              className="settings-segmented-input"
              type="radio"
              name="follow-up-behavior"
              value="steer"
              checked={appSettings.followUpMessageBehavior === "steer"}
              disabled={steerUnavailable}
              onChange={() => {
                if (steerUnavailable) {
                  return;
                }
                void onUpdateAppSettings({
                  ...appSettings,
                  followUpMessageBehavior: "steer",
                });
              }}
            />
            <span className="settings-segmented-option-label">{t("composer.followUp.steer")}</span>
          </label>
        </div>
        <div className="settings-help">
          {t("composer.followUp.help", { shortcut: followUpShortcutLabel })}
        </div>
        {steerUnavailable && (
          <div className="settings-help">
            {t("composer.followUp.unavailableHelp")}
          </div>
        )}
      </div>
      <div className="settings-divider" />
      <div className="settings-subsection-title">{t("composer.presets.title")}</div>
      <div className="settings-subsection-subtitle">
        {t("composer.presets.subtitle")}
      </div>
      <div className="settings-field">
        <label className="settings-field-label" htmlFor="composer-preset">
          {t("composer.presets.label")}
        </label>
        <select
          id="composer-preset"
          className="settings-select"
          value={appSettings.composerEditorPreset}
          onChange={(event) =>
            onComposerPresetChange(event.target.value as ComposerPreset)
          }
        >
          {Object.entries(composerPresetLabels).map(([preset, label]) => (
            <option key={preset} value={preset}>
              {label}
            </option>
          ))}
        </select>
        <div className="settings-help">
          {t("composer.presets.help")}
        </div>
      </div>
      <div className="settings-divider" />
      <div className="settings-subsection-title">{t("composer.fences.title")}</div>
      <SettingsToggleRow
        title={t("composer.fences.space.title")}
        subtitle={t("composer.fences.space.subtitle")}
      >
        <SettingsToggleSwitch
          pressed={appSettings.composerFenceExpandOnSpace}
          onClick={() =>
            void onUpdateAppSettings({
              ...appSettings,
              composerFenceExpandOnSpace: !appSettings.composerFenceExpandOnSpace,
            })
          }
        />
      </SettingsToggleRow>
      <SettingsToggleRow
        title={t("composer.fences.enter.title")}
        subtitle={t("composer.fences.enter.subtitle")}
      >
        <SettingsToggleSwitch
          pressed={appSettings.composerFenceExpandOnEnter}
          onClick={() =>
            void onUpdateAppSettings({
              ...appSettings,
              composerFenceExpandOnEnter: !appSettings.composerFenceExpandOnEnter,
            })
          }
        />
      </SettingsToggleRow>
      <SettingsToggleRow
        title={t("composer.fences.languageTags.title")}
        subtitle={t("composer.fences.languageTags.subtitle")}
      >
        <SettingsToggleSwitch
          pressed={appSettings.composerFenceLanguageTags}
          onClick={() =>
            void onUpdateAppSettings({
              ...appSettings,
              composerFenceLanguageTags: !appSettings.composerFenceLanguageTags,
            })
          }
        />
      </SettingsToggleRow>
      <SettingsToggleRow
        title={t("composer.fences.wrapSelection.title")}
        subtitle={t("composer.fences.wrapSelection.subtitle")}
      >
        <SettingsToggleSwitch
          pressed={appSettings.composerFenceWrapSelection}
          onClick={() =>
            void onUpdateAppSettings({
              ...appSettings,
              composerFenceWrapSelection: !appSettings.composerFenceWrapSelection,
            })
          }
        />
      </SettingsToggleRow>
      <SettingsToggleRow
        title={t("composer.fences.copyWithout.title")}
        subtitle={t("composer.fences.copyWithout.subtitle", { key: optionKeyLabel })}
      >
        <SettingsToggleSwitch
          pressed={appSettings.composerCodeBlockCopyUseModifier}
          onClick={() =>
            void onUpdateAppSettings({
              ...appSettings,
              composerCodeBlockCopyUseModifier:
                !appSettings.composerCodeBlockCopyUseModifier,
            })
          }
        />
      </SettingsToggleRow>
      <div className="settings-divider" />
      <div className="settings-subsection-title">{t("composer.pasting.title")}</div>
      <SettingsToggleRow
        title={t("composer.pasting.multiline.title")}
        subtitle={t("composer.pasting.multiline.subtitle")}
      >
        <SettingsToggleSwitch
          pressed={appSettings.composerFenceAutoWrapPasteMultiline}
          onClick={() =>
            void onUpdateAppSettings({
              ...appSettings,
              composerFenceAutoWrapPasteMultiline:
                !appSettings.composerFenceAutoWrapPasteMultiline,
            })
          }
        />
      </SettingsToggleRow>
      <SettingsToggleRow
        title={t("composer.pasting.codeLike.title")}
        subtitle={t("composer.pasting.codeLike.subtitle")}
      >
        <SettingsToggleSwitch
          pressed={appSettings.composerFenceAutoWrapPasteCodeLike}
          onClick={() =>
            void onUpdateAppSettings({
              ...appSettings,
              composerFenceAutoWrapPasteCodeLike:
                !appSettings.composerFenceAutoWrapPasteCodeLike,
            })
          }
        />
      </SettingsToggleRow>
      <div className="settings-divider" />
      <div className="settings-subsection-title">{t("composer.lists.title")}</div>
      <SettingsToggleRow
        title={t("composer.lists.continue.title")}
        subtitle={t("composer.lists.continue.subtitle")}
      >
        <SettingsToggleSwitch
          pressed={appSettings.composerListContinuation}
          onClick={() =>
            void onUpdateAppSettings({
              ...appSettings,
              composerListContinuation: !appSettings.composerListContinuation,
            })
          }
        />
      </SettingsToggleRow>
    </SettingsSection>
  );
}

import type { AppSettings } from "@/types";
import type { SettingsSection, ShortcutDraftKey, ShortcutSettingKey } from "./settingsTypes";

export const DICTATION_MODELS = [
  { id: "tiny", labelKey: "dictation.models.tiny.label", size: "75 MB", noteKey: "dictation.models.tiny.note" },
  { id: "base", labelKey: "dictation.models.base.label", size: "142 MB", noteKey: "dictation.models.base.note" },
  { id: "small", labelKey: "dictation.models.small.label", size: "466 MB", noteKey: "dictation.models.small.note" },
  { id: "medium", labelKey: "dictation.models.medium.label", size: "1.5 GB", noteKey: "dictation.models.medium.note" },
  {
    id: "large-v3",
    labelKey: "dictation.models.largeV3.label",
    size: "3.0 GB",
    noteKey: "dictation.models.largeV3.note",
  },
] as const;

type ComposerPreset = AppSettings["composerEditorPreset"];

type ComposerPresetSettings = Pick<
  AppSettings,
  | "composerFenceExpandOnSpace"
  | "composerFenceExpandOnEnter"
  | "composerFenceLanguageTags"
  | "composerFenceWrapSelection"
  | "composerFenceAutoWrapPasteMultiline"
  | "composerFenceAutoWrapPasteCodeLike"
  | "composerListContinuation"
  | "composerCodeBlockCopyUseModifier"
>;

export const COMPOSER_PRESET_LABEL_KEYS: Record<ComposerPreset, string> = {
  default: "composer.presets.options.default",
  helpful: "composer.presets.options.helpful",
  smart: "composer.presets.options.smart",
};

export const COMPOSER_PRESET_CONFIGS: Record<
  ComposerPreset,
  ComposerPresetSettings
> = {
  default: {
    composerFenceExpandOnSpace: false,
    composerFenceExpandOnEnter: false,
    composerFenceLanguageTags: false,
    composerFenceWrapSelection: false,
    composerFenceAutoWrapPasteMultiline: false,
    composerFenceAutoWrapPasteCodeLike: false,
    composerListContinuation: false,
    composerCodeBlockCopyUseModifier: false,
  },
  helpful: {
    composerFenceExpandOnSpace: true,
    composerFenceExpandOnEnter: false,
    composerFenceLanguageTags: true,
    composerFenceWrapSelection: true,
    composerFenceAutoWrapPasteMultiline: true,
    composerFenceAutoWrapPasteCodeLike: false,
    composerListContinuation: true,
    composerCodeBlockCopyUseModifier: false,
  },
  smart: {
    composerFenceExpandOnSpace: true,
    composerFenceExpandOnEnter: false,
    composerFenceLanguageTags: true,
    composerFenceWrapSelection: true,
    composerFenceAutoWrapPasteMultiline: true,
    composerFenceAutoWrapPasteCodeLike: true,
    composerListContinuation: true,
    composerCodeBlockCopyUseModifier: false,
  },
};

export const SETTINGS_MOBILE_BREAKPOINT_PX = 720;

export const SETTINGS_SECTION_LABEL_KEYS = {
  projects: "sections.projects",
  archived: "sections.archived",
  environments: "sections.environments",
  skills: "sections.skills",
  mcp: "sections.mcp",
  display: "sections.display",
  about: "sections.about",
  composer: "sections.composer",
  dictation: "sections.dictation",
  shortcuts: "sections.shortcuts",
  "open-apps": "sections.openApps",
  git: "sections.git",
} as const satisfies Record<SettingsSection, string>;

export const SHORTCUT_DRAFT_KEY_BY_SETTING: Record<
  ShortcutSettingKey,
  ShortcutDraftKey
> = {
  composerModelShortcut: "model",
  composerReasoningShortcut: "reasoning",
  interruptShortcut: "interrupt",
  newAgentShortcut: "newAgent",
  newWorktreeAgentShortcut: "newWorktreeAgent",
  newCloneAgentShortcut: "newCloneAgent",
  archiveThreadShortcut: "archiveThread",
  toggleProjectsSidebarShortcut: "projectsSidebar",
  toggleGitSidebarShortcut: "gitSidebar",
  branchSwitcherShortcut: "branchSwitcher",
  toggleDebugPanelShortcut: "debugPanel",
  toggleTerminalShortcut: "terminal",
  cycleAgentNextShortcut: "cycleAgentNext",
  cycleAgentPrevShortcut: "cycleAgentPrev",
  cycleWorkspaceNextShortcut: "cycleWorkspaceNext",
  cycleWorkspacePrevShortcut: "cycleWorkspacePrev",
};

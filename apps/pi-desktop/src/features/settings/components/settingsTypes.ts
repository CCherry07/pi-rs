import type { OpenAppTarget } from "@/types";

export const SETTINGS_SECTION_IDS = [
  "projects",
  "archived",
  "environments",
  "skills",
  "mcp",
  "display",
  "about",
  "composer",
  "dictation",
  "shortcuts",
  "open-apps",
  "git",
] as const;

export const SETTINGS_ROUTE_SECTION_IDS = [
  ...SETTINGS_SECTION_IDS,
  "profile",
] as const;

export type SettingsSection = (typeof SETTINGS_SECTION_IDS)[number];

export type ShortcutSettingKey =
  | "composerModelShortcut"
  | "composerReasoningShortcut"
  | "interruptShortcut"
  | "newAgentShortcut"
  | "newWorktreeAgentShortcut"
  | "newCloneAgentShortcut"
  | "archiveThreadShortcut"
  | "toggleProjectsSidebarShortcut"
  | "toggleGitSidebarShortcut"
  | "branchSwitcherShortcut"
  | "toggleDebugPanelShortcut"
  | "toggleTerminalShortcut"
  | "cycleAgentNextShortcut"
  | "cycleAgentPrevShortcut"
  | "cycleWorkspaceNextShortcut"
  | "cycleWorkspacePrevShortcut";

export type ShortcutDraftKey =
  | "model"
  | "reasoning"
  | "interrupt"
  | "newAgent"
  | "newWorktreeAgent"
  | "newCloneAgent"
  | "archiveThread"
  | "projectsSidebar"
  | "gitSidebar"
  | "branchSwitcher"
  | "debugPanel"
  | "terminal"
  | "cycleAgentNext"
  | "cycleAgentPrev"
  | "cycleWorkspaceNext"
  | "cycleWorkspacePrev";

export type ShortcutDrafts = Record<ShortcutDraftKey, string>;

export type OpenAppDraft = OpenAppTarget & { argsText: string };

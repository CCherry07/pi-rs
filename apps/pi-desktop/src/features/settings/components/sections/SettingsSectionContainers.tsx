import { SettingsSkillsSection } from "./SettingsSkillsSection";
import { SettingsMcpSection } from "./SettingsMcpSection";
import { SettingsComposerSection } from "./SettingsComposerSection";
import { SettingsDictationSection } from "./SettingsDictationSection";
import { SettingsDisplaySection } from "./SettingsDisplaySection";
import { SettingsEnvironmentsSection } from "./SettingsEnvironmentsSection";
import { SettingsGitSection } from "./SettingsGitSection";
import { SettingsOpenAppsSection } from "./SettingsOpenAppsSection";
import { SettingsProjectsSection } from "./SettingsProjectsSection";
import { SettingsArchivedThreadsSection } from "./SettingsArchivedThreadsSection";
import { SettingsShortcutsSection } from "./SettingsShortcutsSection";
import { SettingsAboutSection } from "./SettingsAboutSection";
import type { SettingsSection } from "@settings/components/settingsTypes";
import type { SettingsViewOrchestration } from "@settings/hooks/useSettingsViewOrchestration";

type SettingsSectionContainersProps = {
  activeSection: SettingsSection;
  orchestration: SettingsViewOrchestration;
  onResourceDirtyChange: (dirty: boolean) => void;
};

export function SettingsSectionContainers({
  activeSection,
  orchestration,
  onResourceDirtyChange,
}: SettingsSectionContainersProps) {
  if (activeSection === "mcp") {
    return <SettingsMcpSection projects={orchestration.projectsSectionProps.projects} onDirtyChange={onResourceDirtyChange} />;
  }
  if (activeSection === "skills") {
    return <SettingsSkillsSection projects={orchestration.projectsSectionProps.projects} onDirtyChange={onResourceDirtyChange} />;
  }
  if (activeSection === "projects") {
    return <SettingsProjectsSection {...orchestration.projectsSectionProps} />;
  }
  if (activeSection === "archived") {
    return (
      <SettingsArchivedThreadsSection
        {...orchestration.archivedThreadsSectionProps}
      />
    );
  }
  if (activeSection === "environments") {
    return <SettingsEnvironmentsSection {...orchestration.environmentsSectionProps} />;
  }
  if (activeSection === "display") {
    return <SettingsDisplaySection {...orchestration.displaySectionProps} />;
  }
  if (activeSection === "about") {
    return <SettingsAboutSection {...orchestration.aboutSectionProps} />;
  }
  if (activeSection === "composer") {
    return <SettingsComposerSection {...orchestration.composerSectionProps} />;
  }
  if (activeSection === "dictation") {
    return <SettingsDictationSection {...orchestration.dictationSectionProps} />;
  }
  if (activeSection === "shortcuts") {
    return <SettingsShortcutsSection {...orchestration.shortcutsSectionProps} />;
  }
  if (activeSection === "open-apps") {
    return <SettingsOpenAppsSection {...orchestration.openAppsSectionProps} />;
  }
  if (activeSection === "git") {
    return <SettingsGitSection {...orchestration.gitSectionProps} />;
  }
  return null;
}

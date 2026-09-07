import LayoutGrid from "lucide-react/dist/esm/icons/layout-grid";
import Archive from "lucide-react/dist/esm/icons/archive";
import SlidersHorizontal from "lucide-react/dist/esm/icons/sliders-horizontal";
import Mic from "lucide-react/dist/esm/icons/mic";
import Keyboard from "lucide-react/dist/esm/icons/keyboard";
import GitBranch from "lucide-react/dist/esm/icons/git-branch";
import FileText from "lucide-react/dist/esm/icons/file-text";
import ExternalLink from "lucide-react/dist/esm/icons/external-link";
import Layers from "lucide-react/dist/esm/icons/layers";
import Info from "lucide-react/dist/esm/icons/info";
import { useTranslation } from "react-i18next";
import { PanelNavItem, PanelNavList } from "@/features/design-system/components/panel/PanelPrimitives";
import type { SettingsSection } from "./settingsTypes";
import { SETTINGS_SECTION_LABEL_KEYS } from "./settingsViewConstants";

type SettingsNavProps = {
  activeSection: SettingsSection;
  onSelectSection: (section: SettingsSection) => void;
  showDisclosure?: boolean;
};

export function SettingsNav({
  activeSection,
  onSelectSection,
  showDisclosure = false,
}: SettingsNavProps) {
  const { t } = useTranslation("settings");
  return (
    <aside className="settings-sidebar">
      <PanelNavList className="settings-nav-list">
        <PanelNavItem
          className="settings-nav"
          icon={<LayoutGrid aria-hidden />}
          active={activeSection === "projects"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("projects")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.projects)}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<Archive aria-hidden />}
          active={activeSection === "archived"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("archived")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.archived)}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<Layers aria-hidden />}
          active={activeSection === "environments"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("environments")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.environments)}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<SlidersHorizontal aria-hidden />}
          active={activeSection === "display"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("display")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.display)}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<FileText aria-hidden />}
          active={activeSection === "composer"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("composer")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.composer)}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<Mic aria-hidden />}
          active={activeSection === "dictation"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("dictation")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.dictation)}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<Keyboard aria-hidden />}
          active={activeSection === "shortcuts"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("shortcuts")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.shortcuts)}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<ExternalLink aria-hidden />}
          active={activeSection === "open-apps"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("open-apps")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS["open-apps"])}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<GitBranch aria-hidden />}
          active={activeSection === "git"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("git")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.git)}
        </PanelNavItem>
        <PanelNavItem
          className="settings-nav"
          icon={<Info aria-hidden />}
          active={activeSection === "about"}
          showDisclosure={showDisclosure}
          onClick={() => onSelectSection("about")}
        >
          {t(SETTINGS_SECTION_LABEL_KEYS.about)}
        </PanelNavItem>
      </PanelNavList>
    </aside>
  );
}

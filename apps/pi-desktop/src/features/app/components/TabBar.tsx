import FolderKanban from "lucide-react/dist/esm/icons/folder-kanban";
import GitBranch from "lucide-react/dist/esm/icons/git-branch";
import House from "lucide-react/dist/esm/icons/house";
import MessagesSquare from "lucide-react/dist/esm/icons/messages-square";
import TerminalSquare from "lucide-react/dist/esm/icons/terminal-square";
import { useTranslation } from "react-i18next";

type TabKey = "home" | "projects" | "chat" | "git" | "log";

type TabBarProps = {
  activeTab: TabKey;
  onSelect: (tab: TabKey) => void;
};

export function TabBar({ activeTab, onSelect }: TabBarProps) {
  const { t } = useTranslation("app");
  const tabs = [
    { id: "home", label: t("nav.home"), icon: <House className="tabbar-icon" /> },
    {
      id: "projects",
      label: t("nav.projects"),
      icon: <FolderKanban className="tabbar-icon" />,
    },
    { id: "chat", label: t("nav.chat"), icon: <MessagesSquare className="tabbar-icon" /> },
    { id: "git", label: t("nav.git"), icon: <GitBranch className="tabbar-icon" /> },
    { id: "log", label: t("nav.log"), icon: <TerminalSquare className="tabbar-icon" /> },
  ] satisfies Array<{ id: TabKey; label: string; icon: React.ReactNode }>;
  return (
    <nav className="tabbar" aria-label={t("nav.primary")}>
      {tabs.map((tab) => (
        <button
          key={tab.id}
          type="button"
          className={`tabbar-item ${activeTab === tab.id ? "active" : ""}`}
          onClick={() => onSelect(tab.id)}
          aria-current={activeTab === tab.id ? "page" : undefined}
        >
          {tab.icon}
          <span className="tabbar-label">{tab.label}</span>
        </button>
      ))}
    </nav>
  );
}

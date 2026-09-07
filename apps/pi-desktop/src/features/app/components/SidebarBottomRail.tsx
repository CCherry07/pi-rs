import ScrollText from "lucide-react/dist/esm/icons/scroll-text";
import Settings from "lucide-react/dist/esm/icons/settings";
import { useTranslation } from "react-i18next";

type SidebarBottomRailProps = {
  onOpenSettings: () => void;
  onOpenDebug: () => void;
  showDebugButton: boolean;
};

export function SidebarBottomRail({
  onOpenSettings,
  onOpenDebug,
  showDebugButton,
}: SidebarBottomRailProps) {
  const { t } = useTranslation("app");

  return (
    <div className="sidebar-bottom-rail">
      <div className="sidebar-bottom-actions is-compact">
        <div className="sidebar-utility-actions">
            <button
              className="ghost sidebar-labeled-button sidebar-utility-button"
              type="button"
              onClick={onOpenSettings}
              aria-label={t("sidebar.openSettings")}
            >
              <span className="sidebar-labeled-button-icon" aria-hidden>
                <Settings size={14} aria-hidden />
              </span>
              <span>{t("sidebar.settings")}</span>
            </button>
          {showDebugButton && (
            <button
              className="ghost sidebar-utility-button"
              type="button"
              onClick={onOpenDebug}
              aria-label={t("sidebar.openDebug")}
            >
              <ScrollText size={14} aria-hidden />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

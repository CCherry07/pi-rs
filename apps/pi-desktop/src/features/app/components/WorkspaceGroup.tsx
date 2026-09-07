import { useTranslation } from "react-i18next";

type WorkspaceGroupProps = {
  toggleId: string | null;
  name: string;
  showHeader: boolean;
  isCollapsed: boolean;
  onToggleCollapse: (groupId: string) => void;
  children: React.ReactNode;
};

export function WorkspaceGroup({
  toggleId,
  name,
  showHeader,
  isCollapsed,
  onToggleCollapse,
  children,
}: WorkspaceGroupProps) {
  const { t } = useTranslation("app");
  const isToggleable = Boolean(toggleId);
  const toggleLabel = isCollapsed
    ? t("workspaceGroup.expand")
    : t("workspaceGroup.collapse");
  return (
    <div className="workspace-group">
      {showHeader && (
        <div
          className={`workspace-group-header${isToggleable ? " is-toggleable" : ""}`}
          onClick={
            toggleId
              ? () => {
                  onToggleCollapse(toggleId);
                }
              : undefined
          }
          onKeyDown={
            toggleId
              ? (event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onToggleCollapse(toggleId);
                  }
                }
              : undefined
          }
          role={isToggleable ? "button" : undefined}
          aria-label={isToggleable ? toggleLabel : undefined}
          aria-expanded={isToggleable ? !isCollapsed : undefined}
          tabIndex={isToggleable ? 0 : undefined}
        >
          <div className="workspace-group-label">{name}</div>
          {isToggleable && (
            <button
              className={`group-toggle ${isCollapsed ? "" : "expanded"}`}
              onClick={(event) => {
                event.stopPropagation();
                if (!toggleId) {
                  return;
                }
                onToggleCollapse(toggleId);
              }}
              aria-label={toggleLabel}
              aria-expanded={!isCollapsed}
              type="button"
            >
              <span className="group-toggle-icon">›</span>
            </button>
          )}
        </div>
      )}
      <div className={`workspace-group-list ${isCollapsed ? "collapsed" : ""}`}>
        <div className="workspace-group-content">{children}</div>
      </div>
    </div>
  );
}

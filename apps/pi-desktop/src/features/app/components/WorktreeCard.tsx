import type { MouseEvent } from "react";
import { useTranslation } from "react-i18next";
import Ellipsis from "lucide-react/dist/esm/icons/ellipsis";
import type { WorkspaceInfo } from "../../../types";
import { MenuTrigger } from "../../design-system/components/popover/PopoverPrimitives";
import { SidebarHoverCard } from "./SidebarHoverCard";
import { WorkspaceHoverContents, type WorkspaceHoverAction } from "./WorkspaceHoverContents";

type WorktreeCardProps = {
  worktree: WorkspaceInfo;
  isActive: boolean;
  isDeleting?: boolean;
  actions: WorkspaceHoverAction[];
  onSelectWorkspace: (id: string) => void;
  onShowWorktreeMenu: (event: MouseEvent, worktree: WorkspaceInfo) => void;
  onToggleWorkspaceCollapse: (workspaceId: string, collapsed: boolean) => void;
  children?: React.ReactNode;
};

export function WorktreeCard({
  worktree, isActive, isDeleting = false, actions, onSelectWorkspace,
  onShowWorktreeMenu, onToggleWorkspaceCollapse, children,
}: WorktreeCardProps) {
  const { t } = useTranslation("app");
  const collapsed = worktree.settings.sidebarCollapsed;
  const label = worktree.name?.trim() || worktree.worktree?.branch || worktree.path;
  return <div className={`worktree-card${isDeleting ? " deleting" : ""}`}>
    <SidebarHoverCard label={label} disabled={isDeleting} content={(close) =>
      <WorkspaceHoverContents workspace={worktree} actions={actions} onClose={close} />
    }>
      {({ isOpen, panelId, close, toggle }) => <div
        className={`worktree-row ${isActive ? "active" : ""}${isDeleting ? " deleting" : ""}`}
        role="button" tabIndex={isDeleting ? -1 : 0} aria-disabled={isDeleting}
        onClick={() => { if (!isDeleting) { close(); onSelectWorkspace(worktree.id); } }}
        onContextMenu={(event) => { if (!isDeleting) { close(); onShowWorktreeMenu(event, worktree); } }}
        onKeyDown={(event) => {
          if (!isDeleting && event.target === event.currentTarget && (event.key === "Enter" || event.key === " ")) {
            event.preventDefault(); close(); onSelectWorkspace(worktree.id);
          }
        }}>
        <div className="workspace-title">
          <button type="button" className={`worktree-toggle ${collapsed ? "" : "expanded"}`}
            disabled={isDeleting} onClick={(event) => {
              event.stopPropagation(); close(); onToggleWorkspaceCollapse(worktree.id, !collapsed);
            }} data-tauri-drag-region="false"
            aria-label={collapsed ? t("workspaceCard.showAgents") : t("workspaceCard.hideAgents")}
            aria-expanded={!collapsed}>
            <span className="worktree-toggle-icon">›</span>
          </button>
          <div className="worktree-label">{label}</div>
        </div>
        <div className="worktree-actions">
          {isDeleting ? <div className="worktree-deleting" role="status" aria-live="polite">
            <span className="worktree-deleting-spinner" aria-hidden />
            <span className="worktree-deleting-label">{t("workspaceCard.deleting")}</span>
          </div> : <MenuTrigger className="sidebar-details-trigger" isOpen={isOpen} popupRole="dialog"
            aria-label={t("sidebar.details.show")} aria-controls={isOpen ? panelId : undefined}
            onClick={(event) => { event.stopPropagation(); toggle(true); }}>
            <Ellipsis aria-hidden />
          </MenuTrigger>}
        </div>
      </div>}
    </SidebarHoverCard>
    <div className={`worktree-card-content${collapsed ? " collapsed" : ""}`}
      aria-hidden={collapsed} inert={collapsed ? true : undefined}>
      <div className="worktree-card-content-inner">{children}</div>
    </div>
  </div>;
}

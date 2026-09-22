import type { MouseEvent } from "react";
import { useTranslation } from "react-i18next";
import Ellipsis from "lucide-react/dist/esm/icons/ellipsis";
import type { WorkspaceInfo } from "../../../types";
import { MenuTrigger } from "../../design-system/components/popover/PopoverPrimitives";
import { SidebarHoverCard } from "./SidebarHoverCard";
import { WorkspaceHoverContents, type WorkspaceHoverAction } from "./WorkspaceHoverContents";

type WorkspaceCardProps = {
  workspace: WorkspaceInfo;
  workspaceName?: React.ReactNode;
  summary?: string | null;
  isActive: boolean;
  isCollapsed: boolean;
  actions: WorkspaceHoverAction[];
  onSelectWorkspace: (id: string) => void;
  onShowWorkspaceMenu: (event: MouseEvent, workspaceId: string) => void;
  onToggleWorkspaceCollapse: (workspaceId: string, collapsed: boolean) => void;
  children?: React.ReactNode;
};

export function WorkspaceCard({
  workspace, workspaceName, summary = null, isActive, isCollapsed, actions,
  onSelectWorkspace, onShowWorkspaceMenu, onToggleWorkspaceCollapse, children,
}: WorkspaceCardProps) {
  const { t } = useTranslation("app");
  return <div className="workspace-card">
    <SidebarHoverCard label={workspace.name} content={(close) =>
      <WorkspaceHoverContents workspace={workspace} summary={summary} actions={actions} onClose={close} />
    }>
      {({ isOpen, panelId, close, toggle }) => <div
        className={`workspace-row ${isActive ? "active" : ""}`}
        role="button" tabIndex={0}
        onClick={() => { close(); onSelectWorkspace(workspace.id); }}
        onContextMenu={(event) => { close(); onShowWorkspaceMenu(event, workspace.id); }}
        onKeyDown={(event) => {
          if (event.target === event.currentTarget && (event.key === "Enter" || event.key === " ")) {
            event.preventDefault(); close(); onSelectWorkspace(workspace.id);
          }
        }}>
        <div className="workspace-title">
          <button type="button" className={`workspace-toggle ${isCollapsed ? "" : "expanded"}`}
            onClick={(event) => {
              event.stopPropagation(); close(); onToggleWorkspaceCollapse(workspace.id, !isCollapsed);
            }} data-tauri-drag-region="false"
            aria-label={isCollapsed ? t("workspaceCard.showAgents") : t("workspaceCard.hideAgents")}
            aria-expanded={!isCollapsed}>
            <span className="workspace-toggle-icon">›</span>
          </button>
          <span className="workspace-name">{workspaceName ?? workspace.name}</span>
        </div>
        <div className="workspace-actions">
          <MenuTrigger className="sidebar-details-trigger" isOpen={isOpen} popupRole="dialog"
            aria-label={t("sidebar.details.show")} aria-controls={isOpen ? panelId : undefined}
            onClick={(event) => { event.stopPropagation(); toggle(true); }}>
            <Ellipsis aria-hidden />
          </MenuTrigger>
        </div>
      </div>}
    </SidebarHoverCard>
    <div className={`workspace-card-content${isCollapsed ? " collapsed" : ""}`}
      aria-hidden={isCollapsed} inert={isCollapsed ? true : undefined}>
      <div className="workspace-card-content-inner">{children}</div>
    </div>
  </div>;
}

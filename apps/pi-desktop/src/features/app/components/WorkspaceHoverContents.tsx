import type { WorkspaceInfo } from "../../../types";
import { useTranslation } from "react-i18next";
import { PopoverMenuItem } from "../../design-system/components/popover/PopoverPrimitives";

export type WorkspaceHoverAction = {
  id: string;
  label: string;
  onSelect: () => void | Promise<void>;
  destructive?: boolean;
};

type Props = {
  workspace: WorkspaceInfo;
  summary?: string | null;
  actions: WorkspaceHoverAction[];
  onClose: () => void;
};

export function WorkspaceHoverContents({ workspace, summary, actions, onClose }: Props) {
  const { t } = useTranslation(["app", "workspaces"]);
  const worktree = workspace.kind === "worktree";
  const mode = worktree ? "worktree" : workspace.settings.cloneSourceWorkspaceId ? "clone" : "local";
  const checkoutCount = worktree ? workspace.worktree?.checkoutCount : null;
  const description = [t(`sidebar.details.${mode}`), summary,
    checkoutCount ? t("workspaces:worktree.repositoryCount", { count: checkoutCount }) : null,
  ].filter(Boolean).join(" · ");
  // Worktree project roots describe the source project, not its checkout directories.
  const roots = worktree ? [] : workspace.project?.roots ?? [];
  return <>
    <div className="sidebar-hovercard-summary">{description}</div>
    <dl className="sidebar-hovercard-details">
      {workspace.worktree?.branch && <>
        <dt>{t("sidebar.details.branch")}</dt><dd>{workspace.worktree.branch}</dd>
      </>}
      <dt>{t("sidebar.details.directory")}</dt>
      <dd><code className="sidebar-hovercard-path">{workspace.path}</code></dd>
    </dl>
    {roots.length > 1 && <div className="sidebar-hovercard-roots">
      <div className="sidebar-hovercard-section-label">{t("sidebar.details.roots")}</div>
      {roots.map((root) => <div className="sidebar-hovercard-root" key={root.id}>
        <div className="sidebar-hovercard-root-name"><span>{root.name}</span>{root.id === workspace.project?.primaryRoot &&
          <span className="sidebar-hovercard-root-primary">{t("sidebar.details.primary")}</span>}</div>
        <code className="sidebar-hovercard-path">{root.path}</code>
      </div>)}
    </div>}
    {actions.length > 0 && <div className="sidebar-hovercard-actions">
      {actions.map((action) => <PopoverMenuItem key={action.id}
        className={`sidebar-hovercard-action${action.destructive ? " is-destructive" : ""}`}
        onClick={() => { onClose(); void action.onSelect(); }}>
        {action.label}
      </PopoverMenuItem>)}
    </div>}
  </>;
}

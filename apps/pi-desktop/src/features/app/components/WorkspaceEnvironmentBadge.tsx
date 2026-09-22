import { useState } from "react";
import { useTranslation } from "react-i18next";
import GitBranch from "lucide-react/dist/esm/icons/git-branch";
import Laptop from "lucide-react/dist/esm/icons/laptop";
import Copy from "lucide-react/dist/esm/icons/copy";
import Check from "lucide-react/dist/esm/icons/check";
import type { GitInventory } from "../../git/gitContext";
import { normalizeRootPath } from "../../threads/utils/threadNormalize";
import { MenuTrigger, PopoverSurface } from "../../design-system/components/popover/PopoverPrimitives";
import { useMenuController } from "../hooks/useMenuController";

type Props = {
  inventory: GitInventory | null;
  error?: string | null;
  onRefresh?: () => void | Promise<unknown>;
};

function isWorktree(inventory: GitInventory) {
  const { workspace } = inventory;
  const primary = workspace.roots.find((root) => root.id === workspace.primaryRoot);
  if (primary?.ownership.kind === "managedWorktree") return true;
  const cwd = normalizeRootPath(workspace.executionDir);
  const checkout = inventory.checkouts
    .filter((entry) => {
      const path = normalizeRootPath(entry.workdir);
      return entry.rootIds.includes(workspace.primaryRoot) && (cwd === path || cwd.startsWith(`${path}/`));
    })
    .sort((left, right) => right.workdir.length - left.workdir.length)[0];
  return Boolean(checkout && checkout.gitDir !== checkout.commonDir);
}

export function WorkspaceEnvironmentBadge({ inventory, error, onRefresh }: Props) {
  const { t } = useTranslation("app");
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState<string | null>(null);
  const menu = useMenuController({ onDismiss: () => { setCopied(false); setCopyError(null); } });
  const mode = inventory ? (isWorktree(inventory) ? "worktree" : "local") : null;
  const label = t(`header.environment.${mode ?? (error ? "unavailable" : "loading")}`);
  const path = inventory?.workspace.executionDir;
  const Icon = mode === "worktree" ? GitBranch : Laptop;

  return <div className="workspace-environment" ref={menu.containerRef}>
    <MenuTrigger
      className={`workspace-environment-badge${mode ? ` is-${mode}` : ""}`}
      isOpen={menu.isOpen}
      popupRole="dialog"
      onClick={menu.toggle}
      title={path ? `${label} · ${path}` : label}
    >
      <Icon aria-hidden />
      <span>{label}</span>
    </MenuTrigger>
    {menu.isOpen && <PopoverSurface className="workspace-environment-popover" role="dialog"
      aria-label={t("header.environment.title")}>
      {path ? <>
        <span className="worktree-info-label">{t("header.environment.directory")}</span>
        <code className="workspace-environment-path">{path}</code>
        <button type="button" className="ghost workspace-environment-copy" onClick={async () => {
          setCopyError(null);
          try { await navigator.clipboard.writeText(path); setCopied(true); }
          catch (reason) { setCopyError(String(reason)); }
        }}>
          {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
          {t(copied ? "header.environment.copied" : "header.environment.copy")}
        </button>
        {copyError && <span role="alert" className="worktree-info-error">{copyError}</span>}
      </> : <>
        <span role={error ? "alert" : "status"}>{error || label}</span>
        {error && onRefresh && <button type="button" className="ghost workspace-environment-copy"
          onClick={() => void onRefresh()}>{t("header.environment.retry")}</button>}
      </>}
    </PopoverSurface>}
  </div>;
}

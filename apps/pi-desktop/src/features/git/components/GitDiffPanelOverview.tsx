import type { GitPanelMode } from "../types";
import ArrowLeftRight from "lucide-react/dist/esm/icons/arrow-left-right";
import RotateCw from "lucide-react/dist/esm/icons/rotate-cw";
import { useTranslation } from "react-i18next";

type GitMode = GitPanelMode;

type GitPanelModeStatusProps = {
  mode: GitMode;
  diffStatusLabel: string;
  perFileDiffStatusLabel: string;
  logCountLabel: string;
  logSyncLabel: string;
  logUpstreamLabel: string;
  issuesLoading: boolean;
  issuesTotal: number;
  pullRequestsLoading: boolean;
  pullRequestsTotal: number;
};

export function GitPanelModeStatus({
  mode,
  diffStatusLabel,
  perFileDiffStatusLabel,
  logCountLabel,
  logSyncLabel,
  logUpstreamLabel,
  issuesLoading,
  issuesTotal,
  pullRequestsLoading,
  pullRequestsTotal,
}: GitPanelModeStatusProps) {
  const { t } = useTranslation("git");
  if (mode === "diff") {
    return <div className="diff-status">{diffStatusLabel}</div>;
  }

  if (mode === "perFile") {
    return <div className="diff-status">{perFileDiffStatusLabel}</div>;
  }

  if (mode === "log") {
    return (
      <>
        <div className="diff-status">{logCountLabel}</div>
        <div className="git-log-sync">
          <span>{logSyncLabel}</span>
          {logUpstreamLabel && (
            <>
              <span className="git-log-sep">·</span>
              <span>{logUpstreamLabel}</span>
            </>
          )}
        </div>
      </>
    );
  }

  if (mode === "issues") {
    return (
      <>
        <div className="diff-status diff-status-issues">
          <span>{t("panel.issues")}</span>
          {issuesLoading && <span className="git-panel-spinner" aria-hidden />}
        </div>
        <div className="git-log-sync">
          <span>{t("panel.open", { count: issuesTotal })}</span>
        </div>
      </>
    );
  }

  return (
    <>
      <div className="diff-status diff-status-issues">
        <span>{t("panel.pullRequests")}</span>
        {pullRequestsLoading && <span className="git-panel-spinner" aria-hidden />}
      </div>
      <div className="git-log-sync">
        <span>{t("panel.open", { count: pullRequestsTotal })}</span>
      </div>
    </>
  );
}

type GitBranchRowProps = {
  mode: GitMode;
  branchName: string;
  onFetch?: () => void | Promise<void>;
  fetchLoading: boolean;
};

export function GitBranchRow({ mode, branchName, onFetch, fetchLoading }: GitBranchRowProps) {
  const { t } = useTranslation("git");
  if (mode !== "diff" && mode !== "perFile" && mode !== "log") {
    return null;
  }

  return (
    <div className="diff-branch-row">
      <div className="diff-branch-meta">
        <span className="diff-branch-label">{t("panel.branch")}</span>
        <div className="diff-branch">{branchName || t("branches.unknown")}</div>
      </div>
      <button
        type="button"
        className="diff-branch-refresh"
        onClick={() => void onFetch?.()}
        disabled={!onFetch || fetchLoading}
        title={fetchLoading ? t("branches.fetchingTitle") : t("branches.fetch")}
        aria-label={fetchLoading ? t("branches.fetching") : t("branches.fetch")}
      >
        {fetchLoading ? (
          <span className="git-panel-spinner" aria-hidden />
        ) : (
          <RotateCw size={12} aria-hidden />
        )}
      </button>
    </div>
  );
}

type GitRootCurrentPathProps = {
  mode: GitMode;
  hasGitRoot: boolean;
  gitRoot: string | null;
  onScanGitRoots?: () => void;
  gitRootScanLoading: boolean;
};

export function GitRootCurrentPath({
  mode,
  hasGitRoot,
  gitRoot,
  onScanGitRoots,
  gitRootScanLoading,
}: GitRootCurrentPathProps) {
  const { t } = useTranslation("git");
  if (mode === "issues" || !hasGitRoot) {
    return null;
  }

  return (
    <div className="git-root-current">
      <div className="git-root-current-main">
        <span className="git-root-label">{t("panel.repositoryRoot")}</span>
        <span className="git-root-path" title={gitRoot ?? ""}>
          {gitRoot}
        </span>
      </div>
      {onScanGitRoots && (
        <button
          type="button"
          className="ghost git-root-button git-root-button--icon"
          onClick={onScanGitRoots}
          disabled={gitRootScanLoading}
        >
          <ArrowLeftRight className="git-root-button-icon" aria-hidden />
          {t("panel.change")}
        </button>
      )}
    </div>
  );
}

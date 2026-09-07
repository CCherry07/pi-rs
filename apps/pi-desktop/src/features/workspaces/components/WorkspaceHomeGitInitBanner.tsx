type WorkspaceHomeGitInitBannerProps = {
  isLoading: boolean;
  onInitGitRepo: () => void | Promise<void>;
};

export function WorkspaceHomeGitInitBanner({
  isLoading,
  onInitGitRepo,
}: WorkspaceHomeGitInitBannerProps) {
  const { t } = useTranslation(["workspaces", "git"]);
  return (
    <div className="workspace-home-git-banner" role="region" aria-label={t("workspaces:home.gitSetup")}>
      <div className="workspace-home-git-banner-title">
        {t("workspaces:home.gitNotInitialized")}
      </div>
      <div className="workspace-home-git-banner-actions">
        <button
          type="button"
          className="primary"
          onClick={() => void onInitGitRepo()}
          disabled={isLoading}
        >
          {isLoading ? t("git:initialize.initializing") : t("git:initialize.title")}
        </button>
      </div>
    </div>
  );
}
import { useTranslation } from "react-i18next";

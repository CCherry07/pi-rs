type HomeActionsProps = {
  onAddWorkspace: () => void;
  onAddWorkspaceFromUrl: () => void;
};

export function HomeActions({
  onAddWorkspace,
  onAddWorkspaceFromUrl,
}: HomeActionsProps) {
  const { t } = useTranslation("home");
  return (
    <div className="home-actions">
      <button
        className="home-button primary home-add-workspaces-button"
        onClick={onAddWorkspace}
        data-tauri-drag-region="false"
      >
        <span className="home-icon" aria-hidden>
          +
        </span>
        {t("actions.addWorkspaces")}
      </button>
      <button
        className="home-button secondary home-add-workspace-from-url-button"
        onClick={onAddWorkspaceFromUrl}
        data-tauri-drag-region="false"
      >
        <span className="home-icon" aria-hidden>
          ⤓
        </span>
        {t("actions.addFromUrl")}
      </button>
    </div>
  );
}
import { useTranslation } from "react-i18next";

import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { ModalShell } from "../../design-system/components/modal/ModalShell";

type WorkspaceFromUrlPromptProps = {
  url: string;
  destinationPath: string;
  targetFolderName: string;
  error: string | null;
  isBusy: boolean;
  canSubmit: boolean;
  onUrlChange: (value: string) => void;
  onTargetFolderNameChange: (value: string) => void;
  onChooseDestinationPath: () => void;
  onClearDestinationPath: () => void;
  onCancel: () => void;
  onConfirm: () => void;
};

export function WorkspaceFromUrlPrompt({
  url,
  destinationPath,
  targetFolderName,
  error,
  isBusy,
  canSubmit,
  onUrlChange,
  onTargetFolderNameChange,
  onChooseDestinationPath,
  onClearDestinationPath,
  onCancel,
  onConfirm,
}: WorkspaceFromUrlPromptProps) {
  const { t } = useTranslation(["workspaces", "common"]);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <ModalShell
      ariaLabel={t("workspaces:fromUrl.title")}
      className="workspace-from-url-modal"
      cardClassName="workspace-from-url-modal-card"
      onBackdropClick={() => {
        if (!isBusy) {
          onCancel();
        }
      }}
    >
      <div className="workspace-from-url-modal-content">
        <div className="ds-modal-title">{t("workspaces:fromUrl.title")}</div>
        <label className="ds-modal-label" htmlFor="workspace-url-input">
          {t("workspaces:fromUrl.url")}
        </label>
        <input
          id="workspace-url-input"
          ref={inputRef}
          className="ds-modal-input"
          value={url}
          onChange={(event) => onUrlChange(event.target.value)}
          placeholder="https://github.com/org/repo.git"
        />
        <label className="ds-modal-label" htmlFor="workspace-url-target-name">
          {t("workspaces:fromUrl.folderName")}
        </label>
        <input
          id="workspace-url-target-name"
          className="ds-modal-input"
          value={targetFolderName}
          onChange={(event) => onTargetFolderNameChange(event.target.value)}
          placeholder={t("workspaces:fromUrl.folderNamePlaceholder")}
        />
        <label className="ds-modal-label" htmlFor="workspace-url-destination">
          {t("workspaces:fromUrl.destination")}
        </label>
        <div style={{ display: "flex", gap: 8 }}>
          <textarea
            id="workspace-url-destination"
            className="ds-modal-input"
            value={destinationPath}
            placeholder={t("common:notSet")}
            readOnly
            rows={1}
            wrap="off"
          />
          <button type="button" className="ghost ds-modal-button" onClick={onChooseDestinationPath}>
            {t("common:actions.choose")}
          </button>
          <button
            type="button"
            className="ghost ds-modal-button"
            onClick={onClearDestinationPath}
            disabled={destinationPath.trim().length === 0 || isBusy}
          >
            {t("common:actions.clear")}
          </button>
        </div>
        {error && <div className="ds-modal-error">{error}</div>}
        <div className="ds-modal-actions">
          <button className="ghost ds-modal-button" onClick={onCancel} disabled={isBusy}>
            {t("common:actions.cancel")}
          </button>
          <button
            className="primary ds-modal-button"
            onClick={onConfirm}
            disabled={isBusy || !canSubmit}
          >
            {isBusy ? t("workspaces:fromUrl.cloning") : t("workspaces:fromUrl.confirm")}
          </button>
        </div>
      </div>
    </ModalShell>
  );
}

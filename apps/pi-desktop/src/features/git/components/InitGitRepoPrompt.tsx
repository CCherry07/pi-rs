import { useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { ModalShell } from "../../design-system/components/modal/ModalShell";
import { validateBranchName } from "../utils/branchValidation";

type InitGitRepoPromptProps = {
  workspaceName: string;
  branch: string;
  createRemote: boolean;
  repoName: string;
  isPrivate: boolean;
  error?: string | null;
  isBusy?: boolean;
  onBranchChange: (value: string) => void;
  onCreateRemoteChange: (value: boolean) => void;
  onRepoNameChange: (value: string) => void;
  onPrivateChange: (value: boolean) => void;
  onCancel: () => void;
  onConfirm: () => void;
};

export function InitGitRepoPrompt({
  workspaceName,
  branch,
  createRemote,
  repoName,
  isPrivate,
  error = null,
  isBusy = false,
  onBranchChange,
  onCreateRemoteChange,
  onRepoNameChange,
  onPrivateChange,
  onCancel,
  onConfirm,
}: InitGitRepoPromptProps) {
  const { t } = useTranslation(["git", "common"]);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const validationMessages = useMemo(
    () => ({
      dot: t("git:branchValidation.dot"),
      spaces: t("git:branchValidation.spaces"),
      slashEnds: t("git:branchValidation.slashEnds"),
      doubleSlash: t("git:branchValidation.doubleSlash"),
      lock: t("git:branchValidation.lock"),
      doubleDot: t("git:branchValidation.doubleDot"),
      reflog: t("git:branchValidation.reflog"),
      invalidChars: t("git:branchValidation.invalidChars"),
      trailingDot: t("git:branchValidation.trailingDot"),
    }),
    [t],
  );

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const validationError = useMemo(() => {
    const trimmed = branch.trim();
    if (!trimmed) {
      return t("git:initialize.branchRequired");
    }
    return validateBranchName(branch, validationMessages);
  }, [branch, t, validationMessages]);

  const remoteValidationError = useMemo(() => {
    if (!createRemote) {
      return null;
    }
    const trimmed = repoName.trim();
    if (!trimmed) {
      return t("git:initialize.repoRequired");
    }
    if (/\s/.test(trimmed)) {
      return t("git:initialize.repoSpaces");
    }
    return null;
  }, [createRemote, repoName, t]);

  const combinedValidationError = validationError || remoteValidationError;
  const canSubmit = !isBusy && !combinedValidationError;

  return (
    <ModalShell
      className="git-init-modal"
      ariaLabel={t("git:initialize.title")}
      onBackdropClick={() => {
        if (!isBusy) {
          onCancel();
        }
      }}
    >
      <div className="ds-modal-title git-init-modal-title">{t("git:initialize.title")}</div>
      <div className="ds-modal-subtitle git-init-modal-subtitle">
        {t("git:initialize.subtitle", { workspace: workspaceName })}
      </div>

      <label className="ds-modal-label git-init-modal-label" htmlFor="git-init-branch">
        {t("git:initialize.branch")}
      </label>
      <input
        id="git-init-branch"
        ref={inputRef}
        className="ds-modal-input git-init-modal-input"
        value={branch}
        placeholder="main"
        disabled={isBusy}
        onChange={(event) => onBranchChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            if (!isBusy) {
              onCancel();
            }
            return;
          }
          if (event.key === "Enter") {
            event.preventDefault();
            if (canSubmit) {
              onConfirm();
            }
          }
        }}
      />

      <label className="git-init-modal-checkbox-row">
        <input
          type="checkbox"
          className="git-init-modal-checkbox"
          checked={createRemote}
          disabled={isBusy}
          onChange={(event) => onCreateRemoteChange(event.target.checked)}
        />
        <span className="git-init-modal-checkbox-text">
          {t("git:initialize.createRemote")}
        </span>
      </label>

      {createRemote && (
        <div className="git-init-modal-remote">
          <label className="ds-modal-label git-init-modal-label" htmlFor="git-init-repo-name">
            {t("git:initialize.repo")}
          </label>
          <input
            id="git-init-repo-name"
            className="ds-modal-input git-init-modal-input"
            value={repoName}
            placeholder={t("git:initialize.repoPlaceholder")}
            disabled={isBusy}
            onChange={(event) => onRepoNameChange(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                if (!isBusy) {
                  onCancel();
                }
                return;
              }
              if (event.key === "Enter") {
                event.preventDefault();
                if (canSubmit) {
                  onConfirm();
                }
              }
            }}
          />

          <label className="git-init-modal-checkbox-row git-init-modal-checkbox-row--nested">
            <input
              type="checkbox"
              className="git-init-modal-checkbox"
              checked={isPrivate}
              disabled={isBusy}
              onChange={(event) => onPrivateChange(event.target.checked)}
            />
            <span className="git-init-modal-checkbox-text">{t("git:initialize.privateRepo")}</span>
          </label>
        </div>
      )}

      {(error || combinedValidationError) && (
        <div className="ds-modal-error git-init-modal-error">
          {error || combinedValidationError}
        </div>
      )}

      <div className="ds-modal-actions git-init-modal-actions">
        <button
          type="button"
          className="ghost ds-modal-button git-init-modal-button"
          onClick={onCancel}
          disabled={isBusy}
        >
          {t("common:actions.cancel")}
        </button>
        <button
          type="button"
          className="primary ds-modal-button git-init-modal-button"
          onClick={onConfirm}
          disabled={!canSubmit}
        >
          {isBusy ? t("git:initialize.initializing") : t("git:initialize.confirm")}
        </button>
      </div>
    </ModalShell>
  );
}

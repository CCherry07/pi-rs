import { useEffect, useId, useRef } from "react";
import { useTranslation } from "react-i18next";
import Folder from "lucide-react/dist/esm/icons/folder";
import Plus from "lucide-react/dist/esm/icons/plus";
import X from "lucide-react/dist/esm/icons/x";
import type { WorkspaceRoot } from "../../../types";
import { ModalShell } from "../../design-system/components/modal/ModalShell";
import "../../../styles/create-workspace-modal.css";

export type CreateWorkspacePromptProps = {
  mode?: "create" | "edit";
  name: string;
  roots: Pick<WorkspaceRoot, "id" | "name" | "path">[];
  primaryRootId: string | null;
  error: string | null;
  isBusy: boolean;
  isChoosing: boolean;
  isLoading?: boolean;
  onRetryLoad?: () => void;
  onNameChange: (value: string) => void;
  onChooseDirectories: () => void;
  onRemoveDirectory: (rootId: string) => void;
  onPrimaryRootChange: (rootId: string) => void;
  onCancel: () => void;
  onConfirm: () => void;
};

export function CreateWorkspacePrompt({
  mode = "create",
  name,
  roots,
  primaryRootId,
  error,
  isBusy,
  isChoosing,
  isLoading = false,
  onRetryLoad,
  onNameChange,
  onChooseDirectories,
  onRemoveDirectory,
  onPrimaryRootChange,
  onCancel,
  onConfirm,
}: CreateWorkspacePromptProps) {
  const { t } = useTranslation(["workspaces", "common"]);
  const id = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const formRef = useRef<HTMLFormElement>(null);
  const closeDisabled = isBusy || isChoosing;
  const loadFailed = Boolean(error && onRetryLoad);
  const disabled = closeDisabled || isLoading || loadFailed;
  const isEditing = mode === "edit";
  const canSubmit = !disabled && name.trim().length > 0 &&
    roots.length > 0 && primaryRootId !== null && roots.some((root) => root.id === primaryRootId);
  const hasMultipleDirectories = roots.length > 1;

  useEffect(() => {
    const previousFocus = document.activeElement;
    const firstControl = formRef.current?.querySelector<HTMLElement>(
      "input:not(:disabled), button:not(:disabled)",
    );
    (firstControl ?? formRef.current)?.focus();
    return () => {
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) {
        previousFocus.focus();
      }
    };
  }, []);

  useEffect(() => {
    if (!isLoading && !loadFailed) inputRef.current?.focus();
  }, [isLoading, loadFailed]);

  return (
    <ModalShell
      className="create-workspace-modal"
      ariaLabelledBy={`${id}-title`}
      ariaDescribedBy={`${id}-description`}
      onBackdropClick={() => {
        if (!closeDisabled) onCancel();
      }}
    >
      <form
        ref={formRef}
        tabIndex={-1}
        className="create-workspace-form"
        aria-busy={isBusy || isLoading}
        onSubmit={(event) => {
          event.preventDefault();
          if (canSubmit) onConfirm();
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (!closeDisabled) onCancel();
          }
          if (event.key === "Tab") {
            const controls = formRef.current?.querySelectorAll<HTMLElement>(
              "button:not(:disabled), input:not(:disabled)",
            );
            const first = controls?.[0];
            const last = controls?.[controls.length - 1];
            if (event.shiftKey && document.activeElement === first) {
              event.preventDefault();
              last?.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
              event.preventDefault();
              first?.focus();
            }
          }
        }}
      >
        <div className="create-workspace-heading">
          <h2 className="ds-modal-title" id={`${id}-title`}>
            {isEditing
              ? t("workspaces:createWorkspace.editTitle")
              : t("workspaces:createWorkspace.title")}
          </h2>
          <p className="ds-modal-subtitle" id={`${id}-description`}>
            {isEditing
              ? t("workspaces:createWorkspace.editSubtitle")
              : t("workspaces:createWorkspace.subtitle")}
          </p>
        </div>

        <div className="create-workspace-field">
          <label className="ds-modal-label" htmlFor={`${id}-name`}>
            {t("workspaces:createWorkspace.name")}
          </label>
          <input
            ref={inputRef}
            id={`${id}-name`}
            className="ds-modal-input"
            value={name}
            placeholder={t("workspaces:createWorkspace.namePlaceholder")}
            disabled={disabled}
            onChange={(event) => onNameChange(event.target.value)}
          />
        </div>

        <section
          className="create-workspace-directories"
          aria-labelledby={`${id}-directories`}
        >
          <div className="create-workspace-directory-heading">
            <h3 className="ds-modal-label" id={`${id}-directories`}>
              {t("workspaces:createWorkspace.directories")}
            </h3>
            <button
              type="button"
              className="ghost create-workspace-add-directory"
              disabled={disabled}
              onClick={onChooseDirectories}
            >
              <Plus aria-hidden />
              {isChoosing
                ? t("workspaces:createWorkspace.choosing")
                : t("workspaces:createWorkspace.addDirectory")}
            </button>
          </div>
          {isLoading ? (
            <div className="create-workspace-empty" role="status">
              {t("workspaces:createWorkspace.loading")}
            </div>
          ) : loadFailed ? null : roots.length === 0 ? (
            <div className="create-workspace-empty">
              <Folder aria-hidden />
              <p>{t("workspaces:createWorkspace.empty")}</p>
            </div>
          ) : (
            <ul className="create-workspace-directory-list">
              {roots.map((root) => {
                const { path, name: directoryName } = root;
                const isPrimary = primaryRootId === root.id;
                return (
                  <li
                    key={root.id}
                    className={`create-workspace-directory${isPrimary ? " is-primary" : ""}`}
                  >
                    <label className="create-workspace-directory-choice">
                      {hasMultipleDirectories && (
                        <input
                          type="radio"
                          name={`${id}-primary-directory`}
                          checked={isPrimary}
                          disabled={disabled}
                          aria-label={t("workspaces:createWorkspace.selectPrimary", { path })}
                          onChange={() => onPrimaryRootChange(root.id)}
                        />
                      )}
                      <Folder className="create-workspace-directory-icon" aria-hidden />
                      <span className="create-workspace-directory-copy">
                        <span className="create-workspace-directory-name">
                          <span title={directoryName}>{directoryName}</span>
                          {isPrimary && (
                            <span className="create-workspace-primary-badge">
                              {t("workspaces:createWorkspace.primary")}
                            </span>
                          )}
                        </span>
                        <span className="create-workspace-directory-path" title={path}>
                          {path}
                        </span>
                      </span>
                    </label>
                    <button
                      type="button"
                      className="ghost create-workspace-remove-directory"
                      aria-label={t("workspaces:createWorkspace.removeDirectory", { path })}
                      title={t("workspaces:createWorkspace.removeDirectory", { path })}
                      disabled={disabled}
                      onClick={() => onRemoveDirectory(root.id)}
                    >
                      <X aria-hidden />
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
          {hasMultipleDirectories && !isLoading && !loadFailed && (
            <p className="create-workspace-primary-help">
              {t("workspaces:createWorkspace.primaryHelp")}
            </p>
          )}
        </section>

        {error && !isLoading && (
          <div className="ds-modal-error create-workspace-error" role="alert">
            <span>{error}</span>
            {onRetryLoad && (
              <button
                type="button"
                className="ghost ds-modal-button"
                disabled={closeDisabled}
                onClick={onRetryLoad}
              >
                {t("common:actions.retry")}
              </button>
            )}
          </div>
        )}

        <div className="ds-modal-actions create-workspace-actions">
          <button
            type="button"
            className="ghost ds-modal-button"
            disabled={closeDisabled}
            onClick={onCancel}
          >
            {t("common:actions.cancel")}
          </button>
          <button
            type="submit"
            className="primary ds-modal-button"
            disabled={!canSubmit}
          >
            {isBusy
              ? isEditing
                ? t("workspaces:createWorkspace.saving")
                : t("workspaces:createWorkspace.creating")
              : isEditing
                ? t("common:actions.save")
                : t("common:actions.create")}
          </button>
        </div>
      </form>
    </ModalShell>
  );
}

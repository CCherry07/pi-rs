import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { FocusEvent } from "react";
import type { BranchInfo } from "../../../types";
import { ModalShell } from "../../design-system/components/modal/ModalShell";
import { BranchList } from "../../git/components/BranchList";
import { filterBranches } from "../../git/utils/branchSearch";
import type { GitInventory } from "../../git/gitContext";
import type { WorktreePlanPreview } from "../../../services/tauri";
import type { WorktreeCheckoutChoice } from "../hooks/useWorktreePrompt";
import { ManagedWorktreeRecovery } from "./ManagedWorktreeRecovery";

type WorktreePromptProps = {
  workspaceName: string;
  name: string;
  branch: string;
  branchWasEdited?: boolean;
  branchSuggestions?: BranchInfo[];
  copyAgentsMd: boolean;
  setupScript: string;
  scriptError?: string | null;
  error?: string | null;
  onNameChange: (value: string) => void;
  onChange: (value: string) => void;
  onCopyAgentsMdChange: (value: boolean) => void;
  onSetupScriptChange: (value: string) => void;
  onCancel: () => void;
  onConfirm: () => void;
  isBusy?: boolean;
  isSavingScript?: boolean;
  inventory?: GitInventory | null;
  checkouts?: WorktreeCheckoutChoice[];
  executionRootId?: string | null;
  plan?: WorktreePlanPreview | null;
  isLoading?: boolean;
  isPreparing?: boolean;
  onReview?: () => void;
  onCheckoutChange?: (key: string, patch: Partial<Pick<WorktreeCheckoutChoice, "selected" | "branch" | "startPoint">>) => void;
  onExecutionRootChange?: (rootId: string | null) => void;
};

export function WorktreePrompt({
  workspaceName,
  name,
  branch,
  branchWasEdited = false,
  branchSuggestions = [],
  copyAgentsMd,
  setupScript,
  scriptError = null,
  error = null,
  onNameChange,
  onChange,
  onCopyAgentsMdChange,
  onSetupScriptChange,
  onCancel,
  onConfirm,
  isBusy = false,
  isSavingScript = false,
  inventory,
  checkouts = [],
  executionRootId = null,
  plan,
  isLoading = false,
  isPreparing = false,
  onReview,
  onCheckoutChange,
  onExecutionRootChange,
}: WorktreePromptProps) {
  const { t } = useTranslation(["workspaces", "common"]);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const previewRef = useRef<HTMLElement | null>(null);
  const branchContainerRef = useRef<HTMLDivElement | null>(null);
  const branchListRef = useRef<HTMLDivElement | null>(null);
  const [branchMenuOpen, setBranchMenuOpen] = useState(false);
  const [selectedBranchIndex, setSelectedBranchIndex] = useState(0);
  const [didNavigateBranches, setDidNavigateBranches] = useState(false);
  const [optionsOpen, setOptionsOpen] = useState(Boolean(setupScript.trim()));
  const hasMultipleCheckouts = checkouts.length > 1;
  const canReview = checkouts.some((choice) => choice.selected) && checkouts
    .filter((choice) => choice.selected).every((choice) => choice.branch.trim() && choice.startPoint.trim());
  const handleSubmit = () => {
    if (isBusy || isLoading || isPreparing) return;
    if (onReview && !plan) {
      if (canReview) onReview();
    } else {
      onConfirm();
    }
  };

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const planId = plan?.id;
  useEffect(() => {
    if (!planId) return;
    previewRef.current?.scrollIntoView?.({ block: "start" });
    previewRef.current?.focus({ preventScroll: true });
  }, [planId]);

  const filteredBranches = useMemo(() => {
    const query = !branchWasEdited && branchMenuOpen ? "" : branch;
    return filterBranches(branchSuggestions, query, { mode: "fuzzy", whenEmptyLimit: 8 });
  }, [branch, branchMenuOpen, branchSuggestions, branchWasEdited]);

  useEffect(() => {
    if (!branchMenuOpen) {
      return;
    }
    setDidNavigateBranches(false);
    setSelectedBranchIndex(0);
  }, [branchMenuOpen, filteredBranches.length]);

  useEffect(() => {
    if (!branchMenuOpen) {
      return;
    }
    const itemEl = branchListRef.current?.children[selectedBranchIndex] as
      | HTMLElement
      | undefined;
    itemEl?.scrollIntoView({ block: "nearest" });
  }, [branchMenuOpen, selectedBranchIndex]);

  const handleBranchSelect = (branchInfo: BranchInfo) => {
    onChange(branchInfo.name);
    setBranchMenuOpen(false);
    requestAnimationFrame(() => {
      const input = branchContainerRef.current?.querySelector(
        "input",
      ) as HTMLInputElement | null;
      input?.focus();
    });
  };

  const handleBranchContainerBlur = (event: FocusEvent<HTMLDivElement>) => {
    const nextFocus = event.relatedTarget;
    if (!nextFocus) {
      setBranchMenuOpen(false);
      return;
    }
    if (event.currentTarget.contains(nextFocus)) {
      return;
    }
    setBranchMenuOpen(false);
  };

  return (
    <ModalShell
      className="worktree-modal"
      ariaLabel={t("workspaces:worktree.title")}
      onBackdropClick={() => {
        if (!isBusy) {
          onCancel();
        }
      }}
    >
      <div className="ds-modal-title worktree-modal-title">{t("workspaces:worktree.title")}</div>
      <div className="ds-modal-subtitle worktree-modal-subtitle">
        {t("workspaces:worktree.subtitle", { workspace: workspaceName })}
      </div>
      <label className="ds-modal-label worktree-modal-label" htmlFor="worktree-name">
        {t("workspaces:worktree.name")}
      </label>
      <input
        id="worktree-name"
        ref={inputRef}
        className="ds-modal-input worktree-modal-input"
        value={name}
        disabled={isBusy}
        placeholder={t("workspaces:worktree.optional")}
        onChange={(event) => onNameChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            if (!isBusy) {
              onCancel();
            }
          }
          if (event.key === "Enter" && !isBusy) {
            event.preventDefault();
            handleSubmit();
          }
        }}
      />
      {!hasMultipleCheckouts && <>
      <label className="ds-modal-label worktree-modal-label" htmlFor="worktree-branch">
        {t("workspaces:worktree.branch")}
      </label>
      <div
        className="worktree-modal-branch"
        ref={branchContainerRef}
        onFocusCapture={() => setBranchMenuOpen(branchSuggestions.length > 0)}
        onBlurCapture={handleBranchContainerBlur}
      >
        <input
          id="worktree-branch"
          className="ds-modal-input worktree-modal-input"
          value={branch}
          disabled={isBusy}
          onChange={(event) => {
            setDidNavigateBranches(false);
            onChange(event.target.value);
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              if (!isBusy) {
                onCancel();
              }
              return;
            }

            if (!branchMenuOpen || filteredBranches.length === 0) {
              if (event.key === "Enter" && !isBusy) {
                event.preventDefault();
                handleSubmit();
              }
              if (event.key === "ArrowDown") {
                setBranchMenuOpen(true);
              }
              return;
            }

            if (event.key === "ArrowDown") {
              event.preventDefault();
              setDidNavigateBranches(true);
              setSelectedBranchIndex((prev) =>
                prev < filteredBranches.length - 1 ? prev + 1 : prev,
              );
              return;
            }
            if (event.key === "ArrowUp") {
              event.preventDefault();
              setDidNavigateBranches(true);
              setSelectedBranchIndex((prev) => (prev > 0 ? prev - 1 : prev));
              return;
            }
            if (event.key === "Enter") {
              event.preventDefault();
              if (didNavigateBranches) {
                const picked = filteredBranches[selectedBranchIndex];
                if (picked) {
                  handleBranchSelect(picked);
                  return;
                }
              }
              if (!isBusy) {
                handleSubmit();
              }
            }
          }}
        />
        {branchMenuOpen && (
          <BranchList
            branches={filteredBranches}
            currentBranch={null}
            selectedIndex={selectedBranchIndex}
            listClassName="worktree-modal-branch-list"
            listRef={branchListRef}
            itemClassName="worktree-modal-branch-item"
            itemLabelClassName="worktree-modal-branch-item-name"
            selectedItemClassName="selected"
            emptyClassName="worktree-modal-branch-empty"
            emptyText={
              branch.trim().length > 0
                ? t("workspaces:worktree.noMatchingBranches")
                : t("workspaces:worktree.noBranches")
            }
            onMouseEnter={(index) => {
              setDidNavigateBranches(true);
              setSelectedBranchIndex(index);
            }}
            onSelect={handleBranchSelect}
          />
        )}
      </div>
      </>}
      {isLoading && <div className="worktree-modal-hint" role="status">{t("workspaces:worktree.loadingRepositories")}</div>}
      {inventory && <>
        {hasMultipleCheckouts && <div className="worktree-modal-section-title">{t("workspaces:worktree.repositories")}</div>}
        {checkouts.map((choice, index) => (
          <div className="worktree-modal-checkout" key={choice.checkout.key}>
            {hasMultipleCheckouts ? <label className="worktree-modal-checkout-heading">
              <input type="checkbox" checked={choice.selected} disabled={isBusy}
                onChange={(event) => onCheckoutChange?.(choice.checkout.key, { selected: event.target.checked })} />
              <span>{choice.checkout.workdir}</span>
            </label> : <div className="worktree-modal-checkout-heading">{choice.checkout.workdir}</div>}
            {choice.selected && <div className="worktree-modal-checkout-fields">
              {hasMultipleCheckouts && <label className="ds-modal-label">
                {t("workspaces:worktree.branch")}
                <input className="ds-modal-input" value={choice.branch} disabled={isBusy}
                  aria-label={t("workspaces:worktree.repositoryBranch", { repository: choice.checkout.workdir })}
                  onChange={(event) => onCheckoutChange?.(choice.checkout.key, { branch: event.target.value })} />
              </label>}
              <label className="ds-modal-label">
                {t("workspaces:worktree.startPoint")}
                <input className="ds-modal-input" value={choice.startPoint} disabled={isBusy}
                  list={`worktree-refs-${index}`}
                  aria-label={t("workspaces:worktree.repositoryStartPoint", { repository: choice.checkout.workdir })}
                  onChange={(event) => onCheckoutChange?.(choice.checkout.key, { startPoint: event.target.value })} />
                <datalist id={`worktree-refs-${index}`}>
                  <option value="HEAD" />
                  {choice.branches.map((item) => <option key={item.name} value={item.name} />)}
                </datalist>
              </label>
            </div>}
          </div>
        ))}
        {!checkouts.length && <div className="worktree-modal-hint">{t("workspaces:worktree.noRepositories")}</div>}
        {inventory.workspace.roots.length > 1 && <>
          <label className="ds-modal-label" htmlFor="worktree-execution-root">{t("workspaces:worktree.executionRoot")}</label>
          <select id="worktree-execution-root" className="ds-modal-input" value={executionRootId ?? ""}
            disabled={isBusy} onChange={(event) => onExecutionRootChange?.(event.target.value || null)}>
            <option value="">{t("workspaces:worktree.preserveExecution", { path: inventory.workspace.executionDir })}</option>
            {inventory.workspace.roots.map((root) => <option key={root.id} value={root.id}>{root.name} · {root.path}</option>)}
          </select>
        </>}
      </>}
      {plan && <section ref={previewRef} tabIndex={-1} className="worktree-modal-preview" aria-label={t("workspaces:worktree.preview")}>
        <div className="worktree-modal-section-title">{t("workspaces:worktree.preview")}</div>
        <dl className="worktree-modal-mapping">
          {plan.workspace.roots.map((root) => {
            const source = plan.source.roots.find((entry) => entry.id === root.id);
            return <div key={root.id}>
              <dt>{root.name}</dt>
              <dd>{source?.path}</dd>
              <dd>{source?.path === root.path ? t("workspaces:worktree.keptExternal") : `→ ${root.path}`}</dd>
            </div>;
          })}
        </dl>
        <div className="worktree-modal-hint">{t("workspaces:worktree.executionDirectory")}</div>
        <code>{plan.workspace.executionDir}</code>
        {plan.checkouts.map((checkout) => <div className="worktree-modal-hint" key={checkout.destination}>
          {checkout.branch} · {checkout.startOid.slice(0, 12)}
        </div>)}
        {plan.warnings.map((warning, index) => <div className="worktree-modal-hint" key={index}>{warning}</div>)}
      </section>}
      <details className="worktree-modal-options" open={optionsOpen}
        onToggle={(event) => setOptionsOpen(event.currentTarget.open)}>
        <summary>{t("workspaces:worktree.additionalOptions")}</summary>
        <div className="worktree-modal-options-content">
          <div className="worktree-modal-checkbox-row">
            <input
              id="worktree-copy-agents"
              type="checkbox"
              className="worktree-modal-checkbox-input"
              checked={copyAgentsMd}
              disabled={isBusy}
              onChange={(event) => onCopyAgentsMdChange(event.target.checked)}
            />
            <label className="worktree-modal-checkbox-label" htmlFor="worktree-copy-agents">
              {t("workspaces:worktree.copyAgents")}
            </label>
          </div>
          <label className="worktree-modal-section-title" htmlFor="worktree-setup-script">{t("workspaces:worktree.setupScript")}</label>
          <div className="worktree-modal-hint">
            {t("workspaces:worktree.setupHelp")}
          </div>
          <textarea
            id="worktree-setup-script"
            className="ds-modal-textarea worktree-modal-textarea"
            value={setupScript}
            onChange={(event) => onSetupScriptChange(event.target.value)}
            placeholder="pnpm install"
            rows={4}
            disabled={isBusy || isSavingScript}
          />
        </div>
      </details>
      {scriptError && <div className="ds-modal-error worktree-modal-error">{scriptError}</div>}
      {error && <div className="ds-modal-error worktree-modal-error">{error}</div>}
      <ManagedWorktreeRecovery disabled={isBusy || isPreparing} refreshKey={error} />
      <div className="ds-modal-actions worktree-modal-actions">
        <button
          className="ghost ds-modal-button worktree-modal-button"
          onClick={onCancel}
          type="button"
          disabled={isBusy}
        >
          {t("common:actions.cancel")}
        </button>
        <button
          className="primary ds-modal-button worktree-modal-button"
          onClick={handleSubmit}
          type="button"
          disabled={isBusy || isLoading || isPreparing || (onReview ? !canReview : branch.trim().length === 0)}
        >
          {isPreparing ? t("workspaces:worktree.preparing") : onReview && !plan ? t("workspaces:worktree.review") : t("common:actions.create")}
        </button>
      </div>
    </ModalShell>
  );
}

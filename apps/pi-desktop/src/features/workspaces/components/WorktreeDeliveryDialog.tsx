import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { DeliveryChange } from "../../../services/tauri";
import { ModalShell } from "../../design-system/components/modal/ModalShell";
import { canExecuteDelivery, isDeliveryAttemptPending, type useWorktreeDelivery } from "../hooks/useWorktreeDelivery";

type Props = { delivery: ReturnType<typeof useWorktreeDelivery> };

function LocalChanges({ title, changes, truncated, known = true }: { title: string; changes: DeliveryChange[]; truncated: boolean; known?: boolean }) {
  const { t } = useTranslation("workspaces");
  return <section className="delivery-section">
    <h3>{title}</h3>
    {changes.length ? <>
      <div className="delivery-hint">{t("delivery.statusLegend")}</div>
      <ul className="delivery-file-list">
        {changes.map((change, index) => <li key={`${change.path}-${index}`}>
          <code className="delivery-file-status" aria-label={t("delivery.fileStatus", { index: change.indexStatus || "—", worktree: change.worktreeStatus || "—" })}>
            {change.indexStatus || " "}{change.worktreeStatus || " "}
          </code>
          <span>{change.path}</span>
        </li>)}
      </ul>
    </> : <div className="delivery-hint">{t(known ? "delivery.clean" : "delivery.statusUnavailable")}</div>}
    {truncated && <div className="delivery-hint">{t("delivery.truncated")}</div>}
  </section>;
}

export function WorktreeDeliveryDialog({ delivery }: Props) {
  const { t } = useTranslation(["workspaces", "common"]);
  const { state, close, refresh, selectCheckout, selectTarget, preview: loadPreview, execute, inspect, finish } = delivery;
  const closeRef = useRef<HTMLButtonElement | null>(null);
  const previewRef = useRef<HTMLElement | null>(null);
  const historyRef = useRef<HTMLElement | null>(null);
  const wasOperating = useRef(false);
  useEffect(() => { closeRef.current?.focus(); }, []);
  const preview = state?.preview;
  useEffect(() => {
    if (!preview) return;
    previewRef.current?.scrollIntoView?.({ block: "start" });
    previewRef.current?.focus({ preventScroll: true });
  }, [preview]);
  useEffect(() => {
    if (state?.operation) { wasOperating.current = true; return; }
    if (!wasOperating.current || state?.isLoading) return;
    wasOperating.current = false;
    historyRef.current?.scrollIntoView?.({ block: "start" });
    historyRef.current?.focus({ preventScroll: true });
  }, [state?.operation, state?.isLoading]);
  if (!state) return null;
  const { overview, checkoutKey, isLoading, isPreviewing, error, operation, operationError } = state;
  const checkout = overview?.checkouts.find((entry) => entry.key === checkoutKey);
  const targetBranch = checkoutKey ? state.targets[checkoutKey] ?? "" : "";
  const comparison = preview?.comparison;
  const previewWarnings = [...new Set([...(preview?.warnings ?? []), ...(comparison?.warnings ?? [])])];
  const canPreview = checkout && checkout.head && !checkout.error && targetBranch && !isLoading && !isPreviewing && !operation;
  return <ModalShell className="worktree-delivery-modal" ariaLabel={t("workspaces:delivery.title")} onBackdropClick={close}>
    <div className="delivery-dialog-content" onKeyDown={(event) => {
      if (event.key === "Escape") { event.preventDefault(); close(); }
    }}>
      <div className="delivery-heading">
        <div>
          <h2 className="ds-modal-title">{t("workspaces:delivery.title")}</h2>
          <div className="ds-modal-subtitle">{state.name}</div>
        </div>
        <button ref={closeRef} className="ghost ds-modal-button" type="button" onClick={close}>{t("common:actions.close")}</button>
      </div>
      <div className="delivery-hint">{t("workspaces:delivery.snapshotHelp")}</div>
      <div className="delivery-toolbar">
        {overview && <span>{t("workspaces:delivery.repositoryCount", { count: overview.checkouts.length })}</span>}
        <button className="ghost ds-modal-button" type="button" onClick={() => void refresh()} disabled={isLoading || Boolean(operation)}>
          {t("workspaces:delivery.refresh")}
        </button>
      </div>
      {isLoading && <div className="delivery-hint" role="status">{t("workspaces:delivery.loading")}</div>}
      {error && <div className="ds-modal-error" role="alert">{error}</div>}
      {operationError && <div className="ds-modal-error" role="alert">{operationError}</div>}
      {operation && <div className="delivery-hint" role="status">{t(`workspaces:delivery.operation.${operation.kind}`)} {t("workspaces:delivery.continuesAfterClose")}</div>}
      {overview && <>
        <div className="delivery-repositories" aria-label={t("workspaces:delivery.repositories")}>
          {overview.checkouts.map((entry) => <button className={`delivery-repository${entry.key === checkoutKey ? " selected" : ""}`}
            type="button" key={entry.key} aria-pressed={entry.key === checkoutKey} disabled={isLoading || Boolean(operation)}
            onClick={() => selectCheckout(entry.key)}>
            <strong>{entry.head?.branch ?? (entry.head ? t("workspaces:delivery.detached") : entry.createdBranch)}</strong>
            <span>{entry.workdir}</span>
            <span>{entry.error || !entry.head ? t("workspaces:delivery.unavailable") : entry.changes.length || entry.changesTruncated
              ? t(entry.changesTruncated ? "workspaces:delivery.changedFilesAtLeast" : "workspaces:delivery.changedFiles", { count: entry.changes.length }) : t("workspaces:delivery.clean")}</span>
          </button>)}
        </div>
        {!overview.checkouts.length && <div className="delivery-hint">{t("workspaces:delivery.noRepositories")}</div>}
        {checkout && <>
          <dl className="delivery-paths">
            <div><dt>{t("workspaces:delivery.source")}</dt><dd>{checkout.workdir}</dd></div>
            <div><dt>{t("workspaces:delivery.origin")}</dt><dd>{checkout.originWorkdir}</dd></div>
            <div><dt>{t("workspaces:delivery.currentHead")}</dt><dd>{checkout.head
              ? <>{checkout.head.branch ?? t("workspaces:delivery.detached")} <code title={checkout.head.oid}>{checkout.head.oid.slice(0, 12)}</code></>
              : t("workspaces:delivery.unavailable")}</dd></div>
            <div><dt>{t("workspaces:delivery.createdFrom")}</dt><dd>{checkout.createdBranch} <code title={checkout.startOid}>{checkout.startOid.slice(0, 12)}</code></dd></div>
          </dl>
          {checkout.error && <div className="ds-modal-error" role="alert">{checkout.error}</div>}
          {checkout.warnings.map((message, index) => <div className="delivery-hint" key={index}>{message}</div>)}
          <LocalChanges title={t("workspaces:delivery.sourceChanges")} changes={preview?.sourceChanges ?? checkout.changes}
            truncated={preview?.sourceChangesTruncated ?? checkout.changesTruncated} known={Boolean(preview || (checkout.head && !checkout.error))} />
          <div className="delivery-target-row">
            <label className="ds-modal-label" htmlFor="delivery-target-branch">{t("workspaces:delivery.targetBranch")}
              <select id="delivery-target-branch" className="ds-modal-input" value={targetBranch} disabled={isLoading || Boolean(operation)}
                onChange={(event) => selectTarget(event.target.value)}>
                <option value="">{t("workspaces:delivery.chooseBranch")}</option>
                {checkout.targetBranches.map((branch) => <option key={branch.name} value={branch.name}>{branch.name}</option>)}
              </select>
            </label>
            <button className="primary ds-modal-button" type="button" disabled={!canPreview} onClick={() => void loadPreview()}>
              {isPreviewing ? t("workspaces:delivery.previewing") : t("workspaces:delivery.preview")}
            </button>
          </div>
          {!checkout.targetBranches.length && <div className="delivery-hint">{t("workspaces:delivery.noTargetBranches")}</div>}
        </>}
        {preview && comparison && <section className="delivery-preview" aria-label={t("workspaces:delivery.previewTitle")} ref={previewRef} tabIndex={-1}>
          <h3>{t("workspaces:delivery.previewTitle")}</h3>
          <div className={`delivery-comparison-kind delivery-comparison-${comparison.kind}`}>
            {t(`workspaces:delivery.mergeStatus.${comparison.kind}`)}
          </div>
          <div className="delivery-hint">{t("workspaces:delivery.divergence", { ahead: comparison.ahead, behind: comparison.behind })}</div>
          <dl className="delivery-paths">
            <div><dt>{t("workspaces:delivery.sourceCommit")}</dt><dd>{preview.source.branch ?? t("workspaces:delivery.detached")} <code>{preview.source.oid}</code></dd></div>
            <div><dt>{t("workspaces:delivery.targetCommit")}</dt><dd>{preview.target.branch} <code>{preview.target.oid}</code></dd></div>
            <div><dt>{t("workspaces:delivery.origin")}</dt><dd>{preview.targetWorkdir}</dd></div>
          </dl>
          {(comparison.kind === "fastForward" || comparison.kind === "mergeable") && <div className="delivery-execute">
            <p className="delivery-hint">{t("workspaces:delivery.mergeHelp")}</p>
            <button className="primary ds-modal-button" type="button" disabled={!canExecuteDelivery(state)} onClick={() => void execute()}>
              {t("workspaces:delivery.mergeInto", { branch: preview.target.branch })}
            </button>
            {overview.attempts.some((attempt) => attempt.checkoutKey === checkoutKey && isDeliveryAttemptPending(attempt))
              && <div className="delivery-hint">{t("workspaces:delivery.pendingHelp")}</div>}
          </div>}
          {preview.blockers.length > 0 && <section className="delivery-section delivery-blockers"><h3>{t("workspaces:delivery.blockers")}</h3>
            <ul>{preview.blockers.map((blocker, index) => <li key={index}>{blocker}</li>)}</ul>
          </section>}
          {comparison.conflicts.length > 0 && <section className="delivery-section"><h3>{t("workspaces:delivery.conflictingFiles")}</h3>
            <ul className="delivery-file-list">{comparison.conflicts.map((path) => <li key={path}>{path}</li>)}</ul>
          </section>}
          <section className="delivery-section"><h3>{t("workspaces:delivery.incomingCommits")}</h3>
            {comparison.commits.length ? <ul className="delivery-commits">
              {comparison.commits.map((commit) => <li key={commit.oid}><code title={commit.oid}>{commit.oid.slice(0, 12)}</code><span>{commit.summary}</span></li>)}
            </ul> : <div className="delivery-hint">{t(comparison.kind === "unsupported" ? "workspaces:delivery.commitComparisonUnavailable" : "workspaces:delivery.noIncomingCommits")}</div>}
            {comparison.commitsTruncated && <div className="delivery-hint">{t("workspaces:delivery.truncated")}</div>}
          </section>
          <section className="delivery-section"><h3>{t("workspaces:delivery.incomingFiles")}</h3>
            {comparison.files.length ? <ul className="delivery-file-list">
              {comparison.files.map((file, index) => <li key={`${file.path}-${index}`}><code className="delivery-file-status">{t(`workspaces:delivery.changeStatus.${file.status}`, { defaultValue: file.status })}</code>
                <span>{file.oldPath ? `${file.oldPath} → ${file.path}` : file.path}</span></li>)}
            </ul> : <div className="delivery-hint">{t((comparison.kind === "unrelated" || comparison.kind === "unsupported") && comparison.mergeBaseOids.length !== 1
              ? "workspaces:delivery.fileComparisonUnavailable" : "workspaces:delivery.noIncomingFiles")}</div>}
            {comparison.filesTruncated && <div className="delivery-hint">{t("workspaces:delivery.truncated")}</div>}
          </section>
          <LocalChanges title={t("workspaces:delivery.targetChanges")} changes={preview.targetChanges} truncated={preview.targetChangesTruncated} />
          {previewWarnings.map((warning, index) => <div className="delivery-hint" key={index}>{warning}</div>)}
        </section>}
        {overview.attempts.length > 0 && <section className="delivery-section delivery-history" aria-label={t("workspaces:delivery.history")} ref={historyRef} tabIndex={-1}>
          <h3>{t("workspaces:delivery.history")}</h3>
          {overview.attempts.slice().reverse().map((attempt) => {
            const member = overview.checkouts.find((entry) => entry.key === attempt.checkoutKey);
            const pending = isDeliveryAttemptPending(attempt);
            return <section className="delivery-attempt" key={attempt.id} aria-label={t("workspaces:delivery.attempt", { id: attempt.id })}>
              <div className="delivery-attempt-heading"><strong>{attempt.targetBranch}</strong>
                <span>{t(`workspaces:delivery.attemptStatus.${attempt.status}`)}</span>
              </div>
              <dl className="delivery-paths">
                <div><dt>{t("workspaces:delivery.source")}</dt><dd>{member?.workdir ?? attempt.checkoutKey}</dd></div>
                {member && <div><dt>{t("workspaces:delivery.origin")}</dt><dd>{member.originWorkdir}</dd></div>}
                <div><dt>{t("workspaces:delivery.sourceCommit")}</dt><dd><code>{attempt.sourceOid}</code></dd></div>
                <div><dt>{t("workspaces:delivery.targetCommit")}</dt><dd>{attempt.targetBranch} <code>{attempt.targetOid}</code></dd></div>
                {attempt.resultOid && <div><dt>{t(attempt.status === "completed" ? "workspaces:delivery.resultCommit" : "workspaces:delivery.plannedResultCommit")}</dt><dd><code>{attempt.resultOid}</code></dd></div>}
              </dl>
              {attempt.error && <div className="ds-modal-error">{attempt.error}</div>}
              {attempt.status === "needsAttention" && <p className="delivery-hint">{t("workspaces:delivery.attentionHelp")}</p>}
              {attempt.status === "readyToFinish" && <p className="delivery-hint">{t("workspaces:delivery.finishHelp")}</p>}
              {pending && <div className="delivery-attempt-actions">
                <button className="ghost ds-modal-button" type="button" disabled={isLoading || isPreviewing || Boolean(operation)} onClick={() => void inspect(attempt.id)}>
                  {t("workspaces:delivery.inspect")}
                </button>
                {attempt.status === "readyToFinish" && <button className="primary ds-modal-button" type="button" disabled={isLoading || isPreviewing || Boolean(operation)} onClick={() => void finish(attempt.id)}>
                  {t("workspaces:delivery.finish", { branch: attempt.targetBranch })}
                </button>}
              </div>}
            </section>;
          })}
        </section>}
        {overview.sharedRoots.length > 0 && <details className="delivery-shared-roots">
          <summary>{t("workspaces:delivery.sharedRoots", { count: overview.sharedRoots.length })}</summary>
          <p className="delivery-hint">{t("workspaces:delivery.sharedRootsHelp")}</p>
          <ul>{overview.sharedRoots.map((root) => <li key={root.id}>{root.name} · {root.path}</li>)}</ul>
        </details>}
      </>}
    </div>
  </ModalShell>;
}

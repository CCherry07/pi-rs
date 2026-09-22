import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { confirmWorktreeDiscard, listManagedWorktrees, removeWorktree, type ManagedWorktreeSummary } from "../../../services/tauri";

export function ManagedWorktreeRecovery({ disabled = false, refreshKey }: { disabled?: boolean; refreshKey?: string | null }) {
  const { t } = useTranslation("workspaces");
  const [groups, setGroups] = useState<ManagedWorktreeSummary[]>([]);
  const [removing, setRemoving] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [failedCleanups, setFailedCleanups] = useState<Record<string, string>>({});
  const mounted = useRef(false);
  const requestVersion = useRef(0);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useEffect(() => {
    let current = true;
    const refresh = () => {
      const version = ++requestVersion.current;
      void listManagedWorktrees().then((entries) => {
        if (current && requestVersion.current === version) {
          setGroups(entries.filter((entry) => entry.status !== "prepared" && (entry.status !== "ready" || entry.errors.length > 0)));
          setError(null);
        }
      }).catch((reason) => { if (current && requestVersion.current === version) setError(String(reason)); });
    };
    refresh();
    window.addEventListener("pi-worktree-groups-changed", refresh);
    return () => {
      current = false;
      window.removeEventListener("pi-worktree-groups-changed", refresh);
    };
  }, [refreshKey]);

  async function cleanup(group: ManagedWorktreeSummary, force = false) {
    const { id } = group;
    ++requestVersion.current;
    setRemoving(id);
    setError(null);
    try {
      if (force && !await confirmWorktreeDiscard(group.name)) return;
      if (force) await removeWorktree(id, true);
      else await removeWorktree(id);
      window.dispatchEvent(new Event("pi-project-changed"));
      if (mounted.current) setGroups((entries) => entries.filter((entry) => entry.id !== id));
    } catch (reason) {
      if (mounted.current) setFailedCleanups((previous) => ({ ...previous, [id]: String(reason) }));
    } finally {
      if (mounted.current) setRemoving(null);
    }
  }

  if (!groups.length && !error) return null;
  return <section className="worktree-modal-preview" aria-label={t("worktree.recoveryTitle")}>
    <div className="worktree-modal-section-title">{t("worktree.recoveryTitle")}</div>
    {groups.map((group) => <div className="worktree-modal-recovery" key={group.id}>
      <div>{group.name} · {t("worktree.repositoryCount", { count: group.memberCount })}</div>
      {group.errors.map((message, index) => <div className="worktree-modal-hint" key={index}>{message}</div>)}
      {failedCleanups[group.id] && <div className="ds-modal-error" role="alert">{failedCleanups[group.id]}</div>}
      <button className="ghost ds-modal-button" type="button" disabled={disabled || removing !== null}
        onClick={() => void cleanup(group)}>{t("worktree.retryCleanup")}</button>
      {(group.errors.length > 0 || failedCleanups[group.id]) && <button className="ghost ds-modal-button" type="button"
        disabled={disabled || removing !== null} onClick={() => void cleanup(group, true)}>{t("worktree.discardAndCleanup")}</button>}
    </div>)}
    {error && <div className="ds-modal-error" role="alert">{error}</div>}
  </section>;
}

import { useCallback, useEffect, useMemo } from "react";
import type { BranchInfo, DebugEntry, WorkspaceInfo } from "../../../types";
import { checkoutGitHubPullRequest, checkoutGitBranch, createGitBranch, listGitBranches } from "../../../services/tauri";
import { gitRequestFor } from "../gitContext";
import { useGitOperationScope, useGitScopedState } from "./useGitOperationScope";

type UseGitBranchesOptions = { activeWorkspace: WorkspaceInfo | null; onDebug?: (entry: DebugEntry) => void };
const emptyBranches: BranchInfo[] = [];
export function useGitBranches({ activeWorkspace, onDebug }: UseGitBranchesOptions) {
  const scope = useGitOperationScope(activeWorkspace);
  const [branches, setBranches] = useGitScopedState(scope, emptyBranches);
  const [error, setError] = useGitScopedState<string | null>(scope, null);
  const refreshBranches = useCallback(async () => {
    if (!activeWorkspace || !scope.isCurrent()) return;
    try {
      const response = await listGitBranches(gitRequestFor(activeWorkspace));
      const data = response?.branches ?? response?.result?.branches ?? response ?? [];
      const normalized: BranchInfo[] = Array.isArray(data) ? data.map((item: any) => ({
        name: String(item?.name ?? ""), lastCommit: Number(item?.lastCommit ?? item?.last_commit ?? 0),
      })) : [];
      setBranches(normalized.filter((branch) => branch.name));
      setError(null);
    } catch (err) {
      if (!scope.isCurrent()) return;
      const message = err instanceof Error ? err.message : String(err);
      setError(message);
      onDebug?.({ id: `${Date.now()}-branches-error`, timestamp: Date.now(), source: "error", label: "git/branches/list", payload: message });
    }
  }, [activeWorkspace, onDebug, scope, setBranches, setError]);
  useEffect(() => { void refreshBranches(); }, [refreshBranches]);
  const checkoutBranch = useCallback(async (name: string) => {
    if (!activeWorkspace || !name) return;
    await checkoutGitBranch(gitRequestFor(activeWorkspace), name);
    void refreshBranches();
  }, [activeWorkspace, refreshBranches]);
  const checkoutPullRequest = useCallback(async (number: number) => {
    if (!activeWorkspace || !Number.isFinite(number)) return;
    await checkoutGitHubPullRequest(gitRequestFor(activeWorkspace), number);
    void refreshBranches();
  }, [activeWorkspace, refreshBranches]);
  const createBranch = useCallback(async (name: string) => {
    if (!activeWorkspace || !name) return;
    await createGitBranch(gitRequestFor(activeWorkspace), name);
    void refreshBranches();
  }, [activeWorkspace, refreshBranches]);
  const recentBranches = useMemo(() => branches.slice().sort((a, b) => b.lastCommit - a.lastCommit), [branches]);
  return { branches: recentBranches, error, refreshBranches, checkoutBranch, checkoutPullRequest, createBranch };
}

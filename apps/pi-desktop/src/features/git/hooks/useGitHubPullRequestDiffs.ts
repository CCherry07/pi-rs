import { useGitOperationScope, useGitScopedState } from "./useGitOperationScope";
import { gitRequestFor, gitScopeKey } from "../gitContext";
import { useCallback, useEffect, useRef } from "react";
import type { GitHubPullRequestDiff, WorkspaceInfo } from "../../../types";
import { getGitHubPullRequestDiff } from "../../../services/tauri";

type PullRequestDiffState = {
  diffs: GitHubPullRequestDiff[];
  isLoading: boolean;
  error: string | null;
};

const emptyState: PullRequestDiffState = {
  diffs: [],
  isLoading: false,
  error: null,
};

export function useGitHubPullRequestDiffs(
  activeWorkspace: WorkspaceInfo | null,
  prNumber: number | null,
  enabled: boolean,
) {
  const scope = useGitOperationScope(activeWorkspace);
  const [state, setState] = useGitScopedState<PullRequestDiffState>(scope, emptyState);
  const requestIdRef = useRef(0);
  const workspaceIdRef = useRef<string | null>(gitScopeKey(activeWorkspace));
  const prNumberRef = useRef<number | null>(prNumber ?? null);

  const refresh = useCallback(async () => {
    if (!scope.isCurrent()) return;
    if (!activeWorkspace || !prNumber) {
      setState(emptyState);
      return;
    }
    const workspaceId = gitScopeKey(activeWorkspace)!;
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    setState((prev) => ({ ...prev, isLoading: true, error: null }));
    try {
      const diffs = await getGitHubPullRequestDiff(gitRequestFor(activeWorkspace), prNumber);
      if (
        !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
        workspaceIdRef.current !== workspaceId ||
        prNumberRef.current !== prNumber
      ) {
        return;
      }
      setState({ diffs, isLoading: false, error: null });
    } catch (error) {
      console.error("Failed to load GitHub pull request diff", error);
      if (
        !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
        workspaceIdRef.current !== workspaceId ||
        prNumberRef.current !== prNumber
      ) {
        return;
      }
      setState({
        diffs: [],
        isLoading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }, [activeWorkspace, prNumber, scope, setState]);

  useEffect(() => {
    const workspaceId = gitScopeKey(activeWorkspace);
    if (workspaceIdRef.current !== workspaceId) {
      workspaceIdRef.current = workspaceId;
      requestIdRef.current += 1;
      setState(emptyState);
    }
  }, [activeWorkspace, setState]);

  useEffect(() => {
    if (prNumberRef.current !== prNumber) {
      prNumberRef.current = prNumber ?? null;
      requestIdRef.current += 1;
      setState(emptyState);
    }
  }, [prNumber, setState]);

  useEffect(() => {
    if (!enabled) {
      return;
    }
    void refresh();
  }, [enabled, refresh]);

  return {
    diffs: state.diffs,
    isLoading: state.isLoading,
    error: state.error,
    refresh,
  };
}

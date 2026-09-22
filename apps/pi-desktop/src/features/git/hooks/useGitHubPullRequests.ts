import { useGitOperationScope, useGitScopedState } from "./useGitOperationScope";
import { gitRequestFor, gitScopeKey } from "../gitContext";
import { useCallback, useEffect, useRef } from "react";
import type { GitHubPullRequest, WorkspaceInfo } from "../../../types";
import { getGitHubPullRequests } from "../../../services/tauri";

type GitHubPullRequestsState = {
  pullRequests: GitHubPullRequest[];
  total: number;
  isLoading: boolean;
  error: string | null;
};

const emptyState: GitHubPullRequestsState = {
  pullRequests: [],
  total: 0,
  isLoading: false,
  error: null,
};

export function useGitHubPullRequests(
  activeWorkspace: WorkspaceInfo | null,
  enabled: boolean,
) {
  const scope = useGitOperationScope(activeWorkspace);
  const [state, setState] = useGitScopedState<GitHubPullRequestsState>(scope, emptyState);
  const requestIdRef = useRef(0);
  const workspaceIdRef = useRef<string | null>(gitScopeKey(activeWorkspace));

  const refresh = useCallback(async () => {
    if (!scope.isCurrent()) return;
    if (!activeWorkspace) {
      setState(emptyState);
      return;
    }
    const workspaceId = gitScopeKey(activeWorkspace)!;
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    setState((prev) => ({ ...prev, isLoading: true, error: null }));
    try {
      const response = await getGitHubPullRequests(gitRequestFor(activeWorkspace));
      if (
        !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
        workspaceIdRef.current !== workspaceId
      ) {
        return;
      }
      setState({
        pullRequests: response.pullRequests,
        total: response.total,
        isLoading: false,
        error: null,
      });
    } catch (error) {
      console.error("Failed to load GitHub pull requests", error);
      if (
        !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
        workspaceIdRef.current !== workspaceId
      ) {
        return;
      }
      setState({
        pullRequests: [],
        total: 0,
        isLoading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }, [activeWorkspace, scope, setState]);

  useEffect(() => {
    const workspaceId = gitScopeKey(activeWorkspace);
    if (workspaceIdRef.current !== workspaceId) {
      workspaceIdRef.current = workspaceId;
      requestIdRef.current += 1;
      setState(emptyState);
    }
  }, [activeWorkspace, setState]);

  useEffect(() => {
    if (!enabled) {
      return;
    }
    void refresh();
  }, [enabled, refresh]);

  return {
    pullRequests: state.pullRequests,
    total: state.total,
    isLoading: state.isLoading,
    error: state.error,
    refresh,
  };
}

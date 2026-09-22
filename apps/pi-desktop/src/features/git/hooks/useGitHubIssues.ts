import { useGitOperationScope, useGitScopedState } from "./useGitOperationScope";
import { gitRequestFor, gitScopeKey } from "../gitContext";
import { useCallback, useEffect, useRef } from "react";
import type { GitHubIssue, WorkspaceInfo } from "../../../types";
import { getGitHubIssues } from "../../../services/tauri";

type GitHubIssuesState = {
  issues: GitHubIssue[];
  total: number;
  isLoading: boolean;
  error: string | null;
};

const emptyState: GitHubIssuesState = {
  issues: [],
  total: 0,
  isLoading: false,
  error: null,
};

export function useGitHubIssues(
  activeWorkspace: WorkspaceInfo | null,
  enabled: boolean,
) {
  const scope = useGitOperationScope(activeWorkspace);
  const [state, setState] = useGitScopedState<GitHubIssuesState>(scope, emptyState);
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
      const response = await getGitHubIssues(gitRequestFor(activeWorkspace));
      if (
        !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
        workspaceIdRef.current !== workspaceId
      ) {
        return;
      }
      setState({
        issues: response.issues,
        total: response.total,
        isLoading: false,
        error: null,
      });
    } catch (error) {
      console.error("Failed to load GitHub issues", error);
      if (
        !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
        workspaceIdRef.current !== workspaceId
      ) {
        return;
      }
      setState({
        issues: [],
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
    issues: state.issues,
    total: state.total,
    isLoading: state.isLoading,
    error: state.error,
    refresh,
  };
}

import { useGitOperationScope, useGitScopedState } from "./useGitOperationScope";
import { gitRequestFor, gitScopeKey } from "../gitContext";
import { useCallback, useEffect, useRef } from "react";
import type { GitCommitDiff, WorkspaceInfo } from "../../../types";
import { getGitCommitDiff } from "../../../services/tauri";

type CommitDiffState = {
  diffs: GitCommitDiff[];
  isLoading: boolean;
  error: string | null;
};

const emptyState: CommitDiffState = {
  diffs: [],
  isLoading: false,
  error: null,
};

export function useGitCommitDiffs(
  activeWorkspace: WorkspaceInfo | null,
  sha: string | null,
  enabled: boolean,
  ignoreWhitespaceChanges: boolean,
) {
  const scope = useGitOperationScope(activeWorkspace);
  const [state, setState] = useGitScopedState<CommitDiffState>(scope, emptyState);
  const requestIdRef = useRef(0);
  const workspaceIdRef = useRef<string | null>(gitScopeKey(activeWorkspace));
  const shaRef = useRef<string | null>(sha ?? null);
  const ignoreWhitespaceChangesRef = useRef(ignoreWhitespaceChanges);

  const refresh = useCallback(async () => {
    if (!scope.isCurrent()) return;
    if (!activeWorkspace || !sha) {
      setState(emptyState);
      return;
    }
    const workspaceId = gitScopeKey(activeWorkspace)!;
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    setState((prev) => ({ ...prev, isLoading: true, error: null }));
    try {
      const diffs = await getGitCommitDiff(gitRequestFor(activeWorkspace), sha);
      if (
        !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
        workspaceIdRef.current !== workspaceId ||
        shaRef.current !== sha ||
        ignoreWhitespaceChangesRef.current !== ignoreWhitespaceChanges
      ) {
        return;
      }
      setState({ diffs, isLoading: false, error: null });
    } catch (error) {
      console.error("Failed to load git commit diff", error);
      if (
        !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
        workspaceIdRef.current !== workspaceId ||
        shaRef.current !== sha ||
        ignoreWhitespaceChangesRef.current !== ignoreWhitespaceChanges
      ) {
        return;
      }
      setState({
        diffs: [],
        isLoading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }, [activeWorkspace, ignoreWhitespaceChanges, scope, setState, sha]);

  useEffect(() => {
    const workspaceId = gitScopeKey(activeWorkspace);
    if (workspaceIdRef.current !== workspaceId) {
      workspaceIdRef.current = workspaceId;
      requestIdRef.current += 1;
      setState(emptyState);
    }
  }, [activeWorkspace, setState]);

  useEffect(() => {
    if (shaRef.current !== sha) {
      shaRef.current = sha ?? null;
      requestIdRef.current += 1;
      setState(emptyState);
    }
  }, [setState, sha]);

  useEffect(() => {
    if (ignoreWhitespaceChangesRef.current !== ignoreWhitespaceChanges) {
      ignoreWhitespaceChangesRef.current = ignoreWhitespaceChanges;
      requestIdRef.current += 1;
      setState(emptyState);
    }
  }, [ignoreWhitespaceChanges, setState]);

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

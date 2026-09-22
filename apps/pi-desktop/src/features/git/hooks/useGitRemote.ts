import { useGitOperationScope, useGitScopedState } from "./useGitOperationScope";
import { gitRequestFor, gitScopeKey } from "../gitContext";
import { useCallback, useEffect, useRef } from "react";
import type { WorkspaceInfo } from "../../../types";
import { getGitRemote } from "../../../services/tauri";

type GitRemoteState = {
  remote: string | null;
  error: string | null;
};

const emptyState: GitRemoteState = {
  remote: null,
  error: null,
};

export function useGitRemote(activeWorkspace: WorkspaceInfo | null) {
  const scope = useGitOperationScope(activeWorkspace);
  const [state, setState] = useGitScopedState<GitRemoteState>(scope, emptyState);
  const requestIdRef = useRef(0);
  const workspaceIdRef = useRef<string | null>(gitScopeKey(activeWorkspace));
  const workspaceId = gitScopeKey(activeWorkspace);

  const refresh = useCallback(() => {
    if (!scope.isCurrent()) return;
    if (!workspaceId) {
      setState(emptyState);
      return;
    }

    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;

    return getGitRemote(gitRequestFor(activeWorkspace))
      .then((remote) => {
        if (
          !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
          workspaceIdRef.current !== workspaceId
        ) {
          return;
        }
        setState({ remote, error: null });
      })
      .catch((error) => {
        if (
          !scope.isCurrent() ||
          requestIdRef.current !== requestId ||
          workspaceIdRef.current !== workspaceId
        ) {
          return;
        }
        setState({
          remote: null,
          error: error instanceof Error ? error.message : String(error),
        });
      });
  }, [activeWorkspace, scope, setState, workspaceId]);

  useEffect(() => {
    if (workspaceIdRef.current !== workspaceId) {
      workspaceIdRef.current = workspaceId;
      requestIdRef.current += 1;
      setState(emptyState);
    }

    if (!workspaceId) {
      setState(emptyState);
      return;
    }

    refresh()?.catch(() => {});
  }, [refresh, activeWorkspace, workspaceId, setState]);

  return { ...state, refresh };
}

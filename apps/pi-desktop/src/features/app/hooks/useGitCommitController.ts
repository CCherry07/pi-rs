import { gitRequestFor } from "../../git/gitContext";
import { useGitOperationScope, useGitScopedState } from "../../git/hooks/useGitOperationScope";
import { useCallback, useMemo, useRef } from "react";
import type { WorkspaceInfo } from "../../../types";
import {
  commitGit,
  generateCommitMessage,
  fetchGit,
  pullGit,
  pushGit,
  stageGitAll,
  syncGit,
} from "../../../services/tauri";
import { useGitStatus } from "../../git/hooks/useGitStatus";

type GitStatusState = ReturnType<typeof useGitStatus>["status"];

type GitCommitControllerOptions = {
  activeWorkspace: WorkspaceInfo | null;
  commitMessageModelId: string | null;
  gitStatus: GitStatusState;
  refreshGitStatus: () => void;
  refreshGitLog?: () => void;
};

type GitCommitController = {
  commitMessage: string;
  commitMessageLoading: boolean;
  commitMessageError: string | null;
  commitLoading: boolean;
  pullLoading: boolean;
  fetchLoading: boolean;
  pushLoading: boolean;
  syncLoading: boolean;
  commitError: string | null;
  pullError: string | null;
  fetchError: string | null;
  pushError: string | null;
  syncError: string | null;
  hasWorktreeChanges: boolean;
  onCommitMessageChange: (value: string) => void;
  onGenerateCommitMessage: () => Promise<void>;
  onCommit: () => Promise<void>;
  onCommitAndPush: () => Promise<void>;
  onCommitAndSync: () => Promise<void>;
  onPull: () => Promise<void>;
  onFetch: () => Promise<void>;
  onPush: () => Promise<void>;
  onSync: () => Promise<void>;
};

export function useGitCommitController({
  activeWorkspace,
  commitMessageModelId,
  gitStatus,
  refreshGitStatus,
  refreshGitLog,
}: GitCommitControllerOptions): GitCommitController {
  const messageRevision = useRef(0);
  const scope = useGitOperationScope(activeWorkspace);
  const [commitMessage, setCommitMessage] = useGitScopedState(scope, "");
  const [commitMessageLoading, setCommitMessageLoading] = useGitScopedState(scope, false);
  const [commitMessageError, setCommitMessageError] = useGitScopedState<string | null>(scope,
    null,
  );
  const [commitLoading, setCommitLoading] = useGitScopedState(scope, false);
  const [pullLoading, setPullLoading] = useGitScopedState(scope, false);
  const [fetchLoading, setFetchLoading] = useGitScopedState(scope, false);
  const [pushLoading, setPushLoading] = useGitScopedState(scope, false);
  const [syncLoading, setSyncLoading] = useGitScopedState(scope, false);
  const [commitError, setCommitError] = useGitScopedState<string | null>(scope, null);
  const [pullError, setPullError] = useGitScopedState<string | null>(scope, null);
  const [fetchError, setFetchError] = useGitScopedState<string | null>(scope, null);
  const [pushError, setPushError] = useGitScopedState<string | null>(scope, null);
  const [syncError, setSyncError] = useGitScopedState<string | null>(scope, null);

  const hasWorktreeChanges = useMemo(() => {
    const hasStagedChanges = gitStatus.stagedFiles.length > 0;
    const hasUnstagedChanges = gitStatus.unstagedFiles.length > 0;
    return hasStagedChanges || hasUnstagedChanges;
  }, [gitStatus.stagedFiles.length, gitStatus.unstagedFiles.length]);

  const ensureStagedForCommit = useCallback(async () => {
    const hasStagedChanges = gitStatus.stagedFiles.length > 0;
    const hasUnstagedChanges = gitStatus.unstagedFiles.length > 0;
    if (!activeWorkspace || hasStagedChanges || !hasUnstagedChanges) {
      return;
    }
    await stageGitAll(gitRequestFor(activeWorkspace));
  }, [activeWorkspace, gitStatus.stagedFiles.length, gitStatus.unstagedFiles.length]);

  const handleCommitMessageChange = useCallback((value: string) => {
    messageRevision.current += 1;
    setCommitMessage(value);
  }, [setCommitMessage]);

  const handleGenerateCommitMessage = useCallback(async () => {
    if (!activeWorkspace || commitMessageLoading) {
      return;
    }
    const request = gitRequestFor(activeWorkspace);
    const revision = messageRevision.current;
    setCommitMessageLoading(true);
    setCommitMessageError(null);
    try {
      const message = await generateCommitMessage(request, commitMessageModelId);
      if (!scope.isCurrent()) {
        return;
      }
      if (revision === messageRevision.current) setCommitMessage(message);
    } catch (error) {
      if (!scope.isCurrent()) {
        return;
      }
      setCommitMessageError(
        error instanceof Error ? error.message : String(error),
      );
    } finally {
      if (scope.isCurrent()) {
        setCommitMessageLoading(false);
      }
    }
  }, [activeWorkspace, commitMessageLoading, setCommitMessageLoading, setCommitMessageError, commitMessageModelId, scope, setCommitMessage]);


  const handleCommit = useCallback(async () => {
    if (
      !activeWorkspace ||
      commitLoading ||
      !commitMessage.trim() ||
      !hasWorktreeChanges
    ) {
      return;
    }
    setCommitLoading(true);
    setCommitError(null);
    try {
      await ensureStagedForCommit();
      await commitGit(gitRequestFor(activeWorkspace), commitMessage.trim());
      setCommitMessage("");
      refreshGitStatus();
      refreshGitLog?.();
    } catch (error) {
      setCommitError(error instanceof Error ? error.message : String(error));
    } finally {
      setCommitLoading(false);
    }
  }, [activeWorkspace, commitLoading, commitMessage, ensureStagedForCommit, hasWorktreeChanges, refreshGitLog, refreshGitStatus, setCommitError, setCommitLoading, setCommitMessage]);

  const handleCommitAndPush = useCallback(async () => {
    if (
      !activeWorkspace ||
      commitLoading ||
      pushLoading ||
      !commitMessage.trim() ||
      !hasWorktreeChanges
    ) {
      return;
    }
    let commitSucceeded = false;
    setCommitLoading(true);
    setPushLoading(true);
    setCommitError(null);
    setPushError(null);
    try {
      await ensureStagedForCommit();
      await commitGit(gitRequestFor(activeWorkspace), commitMessage.trim());
      commitSucceeded = true;
      setCommitMessage("");
      setCommitLoading(false);
      await pushGit(gitRequestFor(activeWorkspace));
      refreshGitStatus();
      refreshGitLog?.();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      if (!commitSucceeded) {
        setCommitError(errorMsg);
      } else {
        setPushError(errorMsg);
      }
    } finally {
      setCommitLoading(false);
      setPushLoading(false);
    }
  }, [activeWorkspace, commitLoading, pushLoading, commitMessage, hasWorktreeChanges, setCommitLoading, setPushLoading, setCommitError, setPushError, ensureStagedForCommit, setCommitMessage, refreshGitStatus, refreshGitLog]);

  const handleCommitAndSync = useCallback(async () => {
    if (
      !activeWorkspace ||
      commitLoading ||
      syncLoading ||
      !commitMessage.trim() ||
      !hasWorktreeChanges
    ) {
      return;
    }
    let commitSucceeded = false;
    setCommitLoading(true);
    setSyncLoading(true);
    setCommitError(null);
    setSyncError(null);
    try {
      await ensureStagedForCommit();
      await commitGit(gitRequestFor(activeWorkspace), commitMessage.trim());
      commitSucceeded = true;
      setCommitMessage("");
      setCommitLoading(false);
      await syncGit(gitRequestFor(activeWorkspace));
      refreshGitStatus();
      refreshGitLog?.();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      if (!commitSucceeded) {
        setCommitError(errorMsg);
      } else {
        setSyncError(errorMsg);
      }
    } finally {
      setCommitLoading(false);
      setSyncLoading(false);
    }
  }, [activeWorkspace, commitLoading, syncLoading, commitMessage, hasWorktreeChanges, setCommitLoading, setSyncLoading, setCommitError, setSyncError, ensureStagedForCommit, setCommitMessage, refreshGitStatus, refreshGitLog]);

  const handlePull = useCallback(async () => {
    if (!activeWorkspace || pullLoading) {
      return;
    }
    setPullLoading(true);
    setPullError(null);
    try {
      await pullGit(gitRequestFor(activeWorkspace));
      setPushError(null);
      refreshGitStatus();
      refreshGitLog?.();
    } catch (error) {
      setPullError(error instanceof Error ? error.message : String(error));
    } finally {
      setPullLoading(false);
    }
  }, [activeWorkspace, pullLoading, refreshGitLog, refreshGitStatus, setPullError, setPullLoading, setPushError]);

  const handlePush = useCallback(async () => {
    if (!activeWorkspace || pushLoading) {
      return;
    }
    setPushLoading(true);
    setPushError(null);
    try {
      await pushGit(gitRequestFor(activeWorkspace));
      setPullError(null);
      refreshGitStatus();
      refreshGitLog?.();
    } catch (error) {
      setPushError(error instanceof Error ? error.message : String(error));
    } finally {
      setPushLoading(false);
    }
  }, [activeWorkspace, pushLoading, refreshGitLog, refreshGitStatus, setPullError, setPushError, setPushLoading]);

  const handleFetch = useCallback(async () => {
    if (!activeWorkspace || fetchLoading) {
      return;
    }
    setFetchLoading(true);
    setFetchError(null);
    try {
      await fetchGit(gitRequestFor(activeWorkspace));
      refreshGitStatus();
      refreshGitLog?.();
    } catch (error) {
      setFetchError(error instanceof Error ? error.message : String(error));
    } finally {
      setFetchLoading(false);
    }
  }, [activeWorkspace, fetchLoading, refreshGitLog, refreshGitStatus, setFetchError, setFetchLoading]);

  const handleSync = useCallback(async () => {
    if (!activeWorkspace || syncLoading) {
      return;
    }
    setSyncLoading(true);
    setSyncError(null);
    try {
      await syncGit(gitRequestFor(activeWorkspace));
      setPullError(null);
      setPushError(null);
      setSyncError(null);
      refreshGitStatus();
      refreshGitLog?.();
    } catch (error) {
      setSyncError(error instanceof Error ? error.message : String(error));
    } finally {
      setSyncLoading(false);
    }
  }, [activeWorkspace, refreshGitLog, refreshGitStatus, setPullError, setPushError, setSyncError, setSyncLoading, syncLoading]);

  return {
    commitMessage,
    commitMessageLoading,
    commitMessageError,
    commitLoading,
    pullLoading,
    fetchLoading,
    pushLoading,
    syncLoading,
    commitError,
    pullError,
    fetchError,
    pushError,
    syncError,
    hasWorktreeChanges,
    onCommitMessageChange: handleCommitMessageChange,
    onGenerateCommitMessage: handleGenerateCommitMessage,
    onCommit: handleCommit,
    onCommitAndPush: handleCommitAndPush,
    onCommitAndSync: handleCommitAndSync,
    onPull: handlePull,
    onFetch: handleFetch,
    onPush: handlePush,
    onSync: handleSync,
  };
}

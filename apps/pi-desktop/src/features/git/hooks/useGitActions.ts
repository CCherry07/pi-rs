import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  applyWorktreeChanges as applyWorktreeChangesService,
  createGitHubRepo as createGitHubRepoService,
  initGitRepo as initGitRepoService,
  revertGitAll,
  revertGitFile as revertGitFileService,
  stageGitAll as stageGitAllService,
  stageGitFile as stageGitFileService,
  unstageGitFile as unstageGitFileService,
} from "../../../services/tauri";
import type { WorkspaceInfo } from "../../../types";

type UseGitActionsOptions = {
  activeWorkspace: WorkspaceInfo | null;
  onRefreshGitStatus: () => void;
  onRefreshGitDiffs: () => void;
  onClearGitRootCandidates?: () => void;
  onError?: (error: unknown) => void;
};

export type InitGitRepoOutcome = "initialized" | "cancelled" | "failed";

export function useGitActions({
  activeWorkspace,
  onRefreshGitStatus,
  onRefreshGitDiffs,
  onClearGitRootCandidates,
  onError,
}: UseGitActionsOptions) {
  const { t } = useTranslation(["git", "common"]);
  const [worktreeApplyError, setWorktreeApplyError] = useState<string | null>(null);
  const [worktreeApplyLoading, setWorktreeApplyLoading] = useState(false);
  const [worktreeApplySuccess, setWorktreeApplySuccess] = useState(false);
  const [initGitRepoLoading, setInitGitRepoLoading] = useState(false);
  const [createGitHubRepoLoading, setCreateGitHubRepoLoading] = useState(false);
  const worktreeApplyTimerRef = useRef<number | null>(null);
  const workspaceIdRef = useRef<string | null>(activeWorkspace?.id ?? null);
  const workspaceId = activeWorkspace?.id ?? null;
  const isWorktree = activeWorkspace?.kind === "worktree";

  useEffect(() => {
    workspaceIdRef.current = workspaceId;
  }, [workspaceId]);

  useEffect(() => {
    setWorktreeApplyError(null);
    setWorktreeApplyLoading(false);
    setWorktreeApplySuccess(false);
    setInitGitRepoLoading(false);
    setCreateGitHubRepoLoading(false);
    if (worktreeApplyTimerRef.current) {
      window.clearTimeout(worktreeApplyTimerRef.current);
      worktreeApplyTimerRef.current = null;
    }
  }, [workspaceId]);

  const refreshGitData = useCallback(() => {
    onRefreshGitStatus();
    onRefreshGitDiffs();
  }, [onRefreshGitDiffs, onRefreshGitStatus]);

  const stageGitFile = useCallback(
    async (path: string) => {
      if (!workspaceId) {
        return;
      }
      const actionWorkspaceId = workspaceId;
      try {
        await stageGitFileService(actionWorkspaceId, path);
      } catch (error) {
        onError?.(error);
      } finally {
        if (workspaceIdRef.current === actionWorkspaceId) {
          refreshGitData();
        }
      }
    },
    [onError, refreshGitData, workspaceId],
  );

  const stageGitAll = useCallback(async () => {
    if (!workspaceId) {
      return;
    }
    const actionWorkspaceId = workspaceId;
    try {
      await stageGitAllService(actionWorkspaceId);
    } catch (error) {
      onError?.(error);
    } finally {
      if (workspaceIdRef.current === actionWorkspaceId) {
        refreshGitData();
      }
    }
  }, [onError, refreshGitData, workspaceId]);

  const unstageGitFile = useCallback(
    async (path: string) => {
      if (!workspaceId) {
        return;
      }
      const actionWorkspaceId = workspaceId;
      try {
        await unstageGitFileService(actionWorkspaceId, path);
      } catch (error) {
        onError?.(error);
      } finally {
        if (workspaceIdRef.current === actionWorkspaceId) {
          refreshGitData();
        }
      }
    },
    [onError, refreshGitData, workspaceId],
  );

  const revertGitFile = useCallback(
    async (path: string) => {
      if (!workspaceId) {
        return;
      }
      const actionWorkspaceId = workspaceId;
      try {
        await revertGitFileService(actionWorkspaceId, path);
      } catch (error) {
        onError?.(error);
      } finally {
        if (workspaceIdRef.current === actionWorkspaceId) {
          refreshGitData();
        }
      }
    },
    [onError, refreshGitData, workspaceId],
  );

  const revertAllGitChanges = useCallback(async () => {
    if (!workspaceId) {
      return;
    }
    const confirmed = await ask(
      t("discard.all"),
      { title: t("discard.allTitle"), kind: "warning" },
    );
    if (!confirmed) {
      return;
    }
    try {
      await revertGitAll(workspaceId);
      refreshGitData();
    } catch (error) {
      onError?.(error);
    }
  }, [onError, refreshGitData, t, workspaceId]);

  const applyWorktreeChanges = useCallback(async () => {
    if (!workspaceId || !isWorktree) {
      return;
    }
    const applyWorkspaceId = workspaceId;
    setWorktreeApplyError(null);
    setWorktreeApplySuccess(false);
    setWorktreeApplyLoading(true);
    try {
      await applyWorktreeChangesService(applyWorkspaceId);
      if (workspaceIdRef.current !== applyWorkspaceId) {
        return;
      }
      if (worktreeApplyTimerRef.current) {
        window.clearTimeout(worktreeApplyTimerRef.current);
      }
      setWorktreeApplySuccess(true);
      worktreeApplyTimerRef.current = window.setTimeout(() => {
        if (workspaceIdRef.current !== applyWorkspaceId) {
          return;
        }
        setWorktreeApplySuccess(false);
        worktreeApplyTimerRef.current = null;
      }, 2500);
    } catch (error) {
      if (workspaceIdRef.current !== applyWorkspaceId) {
        return;
      }
      setWorktreeApplyError(
        error instanceof Error ? error.message : String(error),
      );
    } finally {
      if (workspaceIdRef.current === applyWorkspaceId) {
        setWorktreeApplyLoading(false);
      }
    }
  }, [isWorktree, workspaceId]);

  const initGitRepo = useCallback(async (branch: string): Promise<InitGitRepoOutcome> => {
    if (!workspaceId) {
      return "failed";
    }
    const actionWorkspaceId = workspaceId;
    setInitGitRepoLoading(true);
    let shouldRefresh = false;
    let outcome: InitGitRepoOutcome = "failed";
    let commitError: string | null = null;
    try {
      const response = await initGitRepoService(actionWorkspaceId, branch, false);
      if (workspaceIdRef.current !== actionWorkspaceId) {
        return "cancelled";
      }

      if (response.status === "needs_confirmation") {
        const entryCount = response.entryCount ?? 0;
        const confirmed = await ask(
          t("initialize.confirmExisting", { branch, count: entryCount }),
          {
            title: t("initialize.title"),
            kind: "warning",
            okLabel: t("initialize.confirm"),
            cancelLabel: t("common:actions.cancel"),
          },
        );
        if (!confirmed) {
          return "cancelled";
        }

        if (workspaceIdRef.current !== actionWorkspaceId) {
          return "cancelled";
        }

        const forced = await initGitRepoService(actionWorkspaceId, branch, true);
        shouldRefresh = forced.status === "initialized" || forced.status === "already_initialized";
        if (forced.status === "initialized") {
          commitError = forced.commitError ?? null;
        }
        outcome = shouldRefresh ? "initialized" : "failed";
      } else {
        shouldRefresh = response.status === "initialized" || response.status === "already_initialized";
        if (response.status === "initialized") {
          commitError = response.commitError ?? null;
        }
        outcome = shouldRefresh ? "initialized" : "failed";
      }

      if (commitError) {
        onError?.(
          new Error(
            t("initialize.commitFailed", { error: commitError }),
          ),
        );
      }
    } catch (error) {
      onError?.(error);
      outcome = "failed";
    } finally {
      if (workspaceIdRef.current === actionWorkspaceId) {
        setInitGitRepoLoading(false);
        if (shouldRefresh) {
          onClearGitRootCandidates?.();
          refreshGitData();
        }
      }
    }
    return outcome;
  }, [onClearGitRootCandidates, onError, refreshGitData, t, workspaceId]);

  const createGitHubRepo = useCallback(
    async (
      repo: string,
      visibility: "private" | "public",
      branch: string,
    ): Promise<{ ok: true } | { ok: false; error: string }> => {
      if (!workspaceId) {
        return { ok: false, error: t("initialize.noWorkspace") };
      }

      const actionWorkspaceId = workspaceId;
      setCreateGitHubRepoLoading(true);
      try {
        const response = await createGitHubRepoService(
          actionWorkspaceId,
          repo,
          visibility,
          branch,
        );
        if (workspaceIdRef.current !== actionWorkspaceId) {
          return { ok: false, error: t("initialize.workspaceChanged") };
        }

        if (response.status === "ok") {
          return { ok: true };
        }

        const pushError = response.pushError?.trim() ?? "";
        const defaultBranchError = response.defaultBranchError?.trim() ?? "";
        const parts = [];
        if (pushError) {
          parts.push(t("initialize.pushFailed", { error: pushError }));
        }
        if (defaultBranchError) {
          parts.push(t("initialize.defaultBranchFailed", { error: defaultBranchError }));
        }
        const errorMessage =
          parts.length > 0 ? parts.join("\n\n") : t("initialize.remoteIncomplete");
        return { ok: false, error: errorMessage };
      } catch (error) {
        if (workspaceIdRef.current !== actionWorkspaceId) {
          return { ok: false, error: t("initialize.workspaceChanged") };
        }
        return {
          ok: false,
          error: error instanceof Error ? error.message : String(error),
        };
      } finally {
        if (workspaceIdRef.current === actionWorkspaceId) {
          setCreateGitHubRepoLoading(false);
        }
      }
    },
    [t, workspaceId],
  );

  return {
    applyWorktreeChanges,
    createGitHubRepo,
    createGitHubRepoLoading,
    initGitRepo,
    initGitRepoLoading,
    revertAllGitChanges,
    revertGitFile,
    stageGitAll,
    stageGitFile,
    unstageGitFile,
    worktreeApplyError,
    worktreeApplyLoading,
    worktreeApplySuccess,
  };
}

import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { ask, message } from "@tauri-apps/plugin-dialog";
import type { WorkspaceInfo } from "../../../types";
import { pickWorkspacePaths } from "../../../services/tauri";
import type { AddWorkspacesFromPathsResult } from "../../workspaces/hooks/useWorkspaceCrud";

export function useWorkspaceDialogs() {
  const { t } = useTranslation(["app", "common", "workspaces"]);

  const requestWorkspacePaths = useCallback(async () => {
    return pickWorkspacePaths();
  }, []);

  const showAddWorkspacesResult = useCallback(
    async (result: AddWorkspacesFromPathsResult) => {
      const hasIssues =
        result.skippedExisting.length > 0 ||
        result.skippedInvalid.length > 0 ||
        result.failures.length > 0;
      if (!hasIssues) {
        return;
      }

      const lines: string[] = [];
      lines.push(t("workspaceDialogs.added", { count: result.added.length }));
      if (result.skippedExisting.length > 0) {
        lines.push(
          t("workspaceDialogs.skippedExisting", {
            count: result.skippedExisting.length,
          }),
        );
      }
      if (result.skippedInvalid.length > 0) {
        lines.push(
          t("workspaceDialogs.skippedInvalid", {
            count: result.skippedInvalid.length,
          }),
        );
      }
      if (result.failures.length > 0) {
        lines.push(t("workspaceDialogs.failedCount", { count: result.failures.length }));
        const details = result.failures
          .slice(0, 3)
          .map(({ path, message: failureMessage }) => `- ${path}: ${failureMessage}`);
        if (result.failures.length > 3) {
          details.push(t("workspaceDialogs.more", { count: result.failures.length - 3 }));
        }
        lines.push("");
        lines.push(t("workspaceDialogs.failures"));
        lines.push(...details);
      }

      const title =
        result.failures.length > 0
          ? t("workspaceDialogs.someFailed")
          : t("workspaceDialogs.someSkipped");
      await message(lines.join("\n"), {
        title,
        kind: result.failures.length > 0 ? "error" : "warning",
      });
    },
    [t],
  );

  const confirmWorkspaceRemoval = useCallback(
    async (workspaces: WorkspaceInfo[], workspaceId: string) => {
      const workspace = workspaces.find((entry) => entry.id === workspaceId);
      const workspaceName = workspace?.name || t("workspaceDialogs.thisWorkspace");
      const worktreeCount = workspaces.filter(
        (entry) => entry.parentId === workspaceId,
      ).length;
      const detail =
        worktreeCount > 0
          ? `\n\n${t("workspaceDialogs.deleteChildren", { count: worktreeCount })}`
          : "";

      return ask(
        t("workspaceDialogs.deleteWorkspaceQuestion", {
          name: workspaceName,
          detail,
        }),
        {
          title: t("workspaceDialogs.deleteWorkspace"),
          kind: "warning",
          okLabel: t("common:actions.delete"),
          cancelLabel: t("common:actions.cancel"),
        },
      );
    },
    [t],
  );

  const confirmWorktreeRemoval = useCallback(
    async (workspaces: WorkspaceInfo[], workspaceId: string) => {
      const workspace = workspaces.find((entry) => entry.id === workspaceId);
      const workspaceName = workspace?.name || t("workspaceDialogs.thisWorktree");
      return ask(
        t("workspaceDialogs.deleteWorktreeQuestion", { name: workspaceName }),
        {
          title: t("workspaceDialogs.deleteWorktree"),
          kind: "warning",
          okLabel: t("common:actions.delete"),
          cancelLabel: t("common:actions.cancel"),
        },
      );
    },
    [t],
  );

  const showWorkspaceRemovalError = useCallback(
    async (error: unknown) => {
      const errorMessage = error instanceof Error ? error.message : String(error);
      await message(errorMessage, {
        title: t("workspaceDialogs.deleteWorkspaceFailed"),
        kind: "error",
      });
    },
    [t],
  );

  const showWorktreeRemovalError = useCallback(
    async (error: unknown) => {
      const errorMessage = error instanceof Error ? error.message : String(error);
      await message(errorMessage, {
        title: t("workspaceDialogs.deleteWorktreeFailed"),
        kind: "error",
      });
    },
    [t],
  );

  return {
    requestWorkspacePaths,
    showAddWorkspacesResult,
    confirmWorkspaceRemoval,
    confirmWorktreeRemoval,
    showWorkspaceRemovalError,
    showWorktreeRemovalError,
  };
}

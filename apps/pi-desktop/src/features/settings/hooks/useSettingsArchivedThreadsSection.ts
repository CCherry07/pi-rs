import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ask } from "@tauri-apps/plugin-dialog";
import type { ThreadSummary, WorkspaceInfo } from "@/types";
import {
  deleteThread,
  listThreads,
  type ThreadListRequest,
  unarchiveThread,
} from "@services/tauri";

export type ArchivedThreadEntry = ThreadSummary & {
  workspaceId: string;
  workspaceName: string;
  workspacePath: string;
};

export type ArchivedThreadAction = "restore" | "delete";

export type SettingsArchivedThreadsSectionProps = {
  archivedThreads: ArchivedThreadEntry[];
  loading: boolean;
  error: string | null;
  busyThreadActions: Readonly<Record<string, ArchivedThreadAction>>;
  deletingAll: boolean;
  onRefresh: () => void;
  onRestoreThread: (entry: ArchivedThreadEntry) => Promise<void>;
  onDeleteThread: (entry: ArchivedThreadEntry) => Promise<void>;
  onDeleteAllThreads: () => Promise<void>;
};

const toErrorMessage = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

const entryKey = (entry: Pick<ArchivedThreadEntry, "workspaceId" | "id">) =>
  `${entry.workspaceId}:${entry.id}`;

const parseThreadSummary = (value: unknown): ThreadSummary | null => {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  const record = value as Record<string, unknown>;
  const id = typeof record.id === "string" ? record.id.trim() : "";
  if (!id) {
    return null;
  }
  const rawName =
    typeof record.name === "string"
      ? record.name
      : typeof record.preview === "string"
        ? record.preview
        : "";
  const updatedAt =
    typeof record.updatedAt === "number"
      ? record.updatedAt
      : typeof record.createdAt === "number"
        ? record.createdAt
        : 0;
  const messageCount =
    typeof record.messageCount === "number" ? record.messageCount : undefined;

  return {
    id,
    name: rawName.trim(),
    updatedAt,
    createdAt:
      typeof record.createdAt === "number" ? record.createdAt : undefined,
    messageCount,
    modelId: typeof record.modelId === "string" ? record.modelId : null,
    effort: typeof record.effort === "string" ? record.effort : null,
  };
};

export const useSettingsArchivedThreadsSection = ({
  enabled,
  projects,
}: {
  enabled: boolean;
  projects: WorkspaceInfo[];
}): SettingsArchivedThreadsSectionProps => {
  const { t } = useTranslation("settings");
  const { t: tCommon } = useTranslation("common");
  const [archivedThreads, setArchivedThreads] = useState<ArchivedThreadEntry[]>(
    [],
  );
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busyThreadActions, setBusyThreadActions] = useState<
    Record<string, ArchivedThreadAction>
  >({});
  const [deletingAll, setDeletingAll] = useState(false);
  const listingRevision = useRef(0);

  const setThreadAction = (
    key: string,
    action: ArchivedThreadAction | null,
  ) => {
    setBusyThreadActions((current) => {
      const next = { ...current };
      if (action) {
        next[key] = action;
      } else {
        delete next[key];
      }
      return next;
    });
  };

  const loadArchivedThreads = useCallback(async () => {
    const revision = ++listingRevision.current;
    if (!enabled || projects.length === 0) {
      setArchivedThreads([]);
      setLoading(false);
      setError(null);
      return;
    }

    setLoading(true);
    setError(null);
    const nextThreads: ArchivedThreadEntry[] = [];
    const failures: string[] = [];
    const byId = new Map(
      projects.map((workspace) => [workspace.id, workspace]),
    );
    let requests: ThreadListRequest[] = projects.map(({ id }) => ({
      workspaceId: id,
    }));
    try {
      while (requests.length) {
        const pages = await listThreads(requests, {
          limit: 200,
          sortKey: "updated_at",
          archived: true,
        });
        if (revision !== listingRevision.current) return;
        requests = [];
        for (const page of pages) {
          const workspace = byId.get(page.workspaceId);
          if (!workspace) continue;
          for (const row of page.data) {
            const summary = parseThreadSummary(row);
            if (summary)
              nextThreads.push({
                ...summary,
                workspaceId: workspace.id,
                workspaceName: workspace.name,
                workspacePath: workspace.path,
              });
          }
          if (page.nextCursor)
            requests.push({
              workspaceId: workspace.id,
              cursor: page.nextCursor,
            });
        }
      }
    } catch (error) {
      failures.push(toErrorMessage(error));
    }
    if (revision !== listingRevision.current) return;
    nextThreads.sort((left, right) => right.updatedAt - left.updatedAt);
    setArchivedThreads(nextThreads);
    setError(
      failures.length > 0
        ? t("archived.error", { message: failures[0], count: failures.length })
        : null,
    );
    setLoading(false);
  }, [enabled, projects, t]);

  useEffect(() => {
    void loadArchivedThreads();
    return () => {
      listingRevision.current += 1;
    };
  }, [loadArchivedThreads]);

  const handleRestoreThread = async (entry: ArchivedThreadEntry) => {
    const key = entryKey(entry);
    setThreadAction(key, "restore");
    setError(null);
    try {
      await unarchiveThread(entry.workspaceId, entry.id);
      setArchivedThreads((prev) =>
        prev.filter(
          (thread) =>
            thread.workspaceId !== entry.workspaceId || thread.id !== entry.id,
        ),
      );
    } catch (restoreError) {
      setError(toErrorMessage(restoreError));
    } finally {
      setThreadAction(key, null);
    }
  };

  const handleDeleteThread = async (entry: ArchivedThreadEntry) => {
    const confirmed = await ask(
      t("archived.deleteQuestion", { name: entry.name || entry.id }),
      {
        title: t("archived.deleteTitle"),
        kind: "warning",
        okLabel: tCommon("actions.delete"),
        cancelLabel: tCommon("actions.cancel"),
      },
    );
    if (!confirmed) {
      return;
    }
    const key = entryKey(entry);
    setThreadAction(key, "delete");
    setError(null);
    try {
      await deleteThread(entry.workspaceId, entry.id);
      setArchivedThreads((prev) =>
        prev.filter(
          (thread) =>
            thread.workspaceId !== entry.workspaceId || thread.id !== entry.id,
        ),
      );
    } catch (deleteError) {
      setError(toErrorMessage(deleteError));
    } finally {
      setThreadAction(key, null);
    }
  };

  const handleDeleteAllThreads = async () => {
    if (archivedThreads.length === 0) {
      return;
    }
    const confirmed = await ask(
      t("archived.deleteAllQuestion", { count: archivedThreads.length }),
      {
        title: t("archived.deleteAllTitle"),
        kind: "warning",
        okLabel: tCommon("actions.delete"),
        cancelLabel: tCommon("actions.cancel"),
      },
    );
    if (!confirmed) {
      return;
    }

    setDeletingAll(true);
    setError(null);
    const targets = [...archivedThreads];
    const results = await Promise.allSettled(
      targets.map((entry) => deleteThread(entry.workspaceId, entry.id)),
    );
    const deletedKeys = new Set<string>();
    const failures: string[] = [];
    results.forEach((result, index) => {
      const target = targets[index];
      if (!target) {
        return;
      }
      if (result.status === "fulfilled") {
        deletedKeys.add(entryKey(target));
      } else {
        failures.push(toErrorMessage(result.reason));
      }
    });
    setArchivedThreads((prev) =>
      prev.filter((thread) => !deletedKeys.has(entryKey(thread))),
    );
    setError(
      failures.length > 0
        ? t("archived.deleteAllError", {
            message: failures[0],
            count: failures.length,
          })
        : null,
    );
    setDeletingAll(false);
  };

  return {
    archivedThreads,
    loading,
    error,
    busyThreadActions,
    deletingAll,
    onRefresh: () => {
      void loadArchivedThreads();
    },
    onRestoreThread: handleRestoreThread,
    onDeleteThread: handleDeleteThread,
    onDeleteAllThreads: handleDeleteAllThreads,
  };
};

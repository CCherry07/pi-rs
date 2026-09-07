import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { setTrayRecentThreads } from "@services/tauri";
import appI18n from "@/i18n";
import type { ThreadSummary, TrayRecentThreadEntry, WorkspaceInfo } from "../../../types";

const SYNC_DEBOUNCE_MS = 150;

type UseTrayRecentThreadsParams = {
  workspaces: WorkspaceInfo[];
  threadsByWorkspace: Record<string, ThreadSummary[]>;
  isSubagentThread: (workspaceId: string, threadId: string) => boolean;
};

type CandidateThread = {
  workspaceId: string;
  workspaceLabel: string;
  threadId: string;
  threadLabel: string;
  updatedAt: number;
};

function buildCandidateThreads(
  workspaces: WorkspaceInfo[],
  threadsByWorkspace: Record<string, ThreadSummary[]>,
  isSubagentThread: (workspaceId: string, threadId: string) => boolean,
  language?: string,
): CandidateThread[] {
  const translate = (key: "tray.workspace" | "tray.untitledThread") =>
    appI18n.t(key, { ns: "app", ...(language ? { lng: language } : {}) });
  const workspaceLabelById = new Map(
    workspaces.map((workspace) => [
      workspace.id,
      workspace.name.trim() || translate("tray.workspace"),
    ] as const),
  );
  const candidates: CandidateThread[] = [];

  Object.entries(threadsByWorkspace).forEach(([workspaceId, threads]) => {
    const workspaceLabel =
      workspaceLabelById.get(workspaceId) ?? translate("tray.workspace");
    threads.forEach((thread) => {
      const threadId = String(thread.id ?? "").trim();
      if (!threadId || isSubagentThread(workspaceId, threadId)) {
        return;
      }
      candidates.push({
        workspaceId,
        workspaceLabel,
        threadId,
        threadLabel:
          thread.name?.trim() || translate("tray.untitledThread"),
        updatedAt: Number(thread.updatedAt ?? 0),
      });
    });
  });

  candidates.sort((left, right) => {
    return (
      right.updatedAt - left.updatedAt ||
      left.threadLabel.localeCompare(right.threadLabel) ||
      left.workspaceLabel.localeCompare(right.workspaceLabel)
    );
  });

  return candidates;
}

export function buildTrayRecentThreadEntries(
  workspaces: WorkspaceInfo[],
  threadsByWorkspace: Record<string, ThreadSummary[]>,
  isSubagentThread: (workspaceId: string, threadId: string) => boolean,
  language?: string,
): TrayRecentThreadEntry[] {
  const candidates = buildCandidateThreads(
    workspaces,
    threadsByWorkspace,
    isSubagentThread,
    language,
  );

  return candidates.map((candidate) => ({
    workspaceId: candidate.workspaceId,
    workspaceLabel: candidate.workspaceLabel,
    threadId: candidate.threadId,
    threadLabel: candidate.threadLabel,
    updatedAt: candidate.updatedAt,
  }));
}

export function useTrayRecentThreads({
  workspaces,
  threadsByWorkspace,
  isSubagentThread,
}: UseTrayRecentThreadsParams) {
  const { i18n } = useTranslation();
  const language = i18n.resolvedLanguage ?? i18n.language;
  const entries = useMemo(
    // Tauri derives the top-3 recents and workspace submenus from the full visible tray thread set.
    () =>
      buildTrayRecentThreadEntries(
        workspaces,
        threadsByWorkspace,
        isSubagentThread,
        language,
      ),
    [isSubagentThread, language, threadsByWorkspace, workspaces],
  );
  const serializedEntries = useMemo(() => JSON.stringify(entries), [entries]);
  const syncEntriesRef = useRef({ serializedEntries, entries });
  if (syncEntriesRef.current.serializedEntries !== serializedEntries) {
    syncEntriesRef.current = { serializedEntries, entries };
  }
  const syncEntries = syncEntriesRef.current.entries;
  const lastSyncedEntriesRef = useRef<string | null>(null);

  useEffect(() => {
    if (!isTauri()) {
      return;
    }

    if (lastSyncedEntriesRef.current === serializedEntries) {
      return;
    }

    let cancelled = false;
    let timeoutId: number | null = null;

    const scheduleSync = () => {
      timeoutId = window.setTimeout(() => {
        timeoutId = null;
        void setTrayRecentThreads(syncEntries)
          .then(() => {
            if (cancelled) {
              return;
            }
            lastSyncedEntriesRef.current = serializedEntries;
          })
          .catch(() => {
            if (cancelled) {
              return;
            }
            // Retry until the desktop bridge or tray is ready for the same payload.
            scheduleSync();
          });
      }, SYNC_DEBOUNCE_MS);
    };

    scheduleSync();

    return () => {
      cancelled = true;
      if (timeoutId !== null) {
        window.clearTimeout(timeoutId);
      }
    };
  }, [serializedEntries, syncEntries]);
}

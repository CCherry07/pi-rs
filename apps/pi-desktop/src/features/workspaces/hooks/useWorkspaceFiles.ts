import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DebugEntry, WorkspaceInfo } from "../../../types";
import { getWorkspaceFiles } from "../../../services/tauri";

type UseWorkspaceFilesOptions = {
  activeWorkspace: WorkspaceInfo | null;
  threadId?: string | null;
  onDebug?: (entry: DebugEntry) => void;
  enabled?: boolean;
  pollingEnabled?: boolean;
};

function areStringArraysEqual(a: string[], b: string[]) {
  if (a === b) {
    return true;
  }
  if (a.length !== b.length) {
    return false;
  }
  for (let index = 0; index < a.length; index += 1) {
    if (a[index] !== b[index]) {
      return false;
    }
  }
  return true;
}

export function useWorkspaceFiles({
  activeWorkspace,
  threadId,
  onDebug,
  enabled = true,
  pollingEnabled,
}: UseWorkspaceFilesOptions) {
  const [files, setFiles] = useState<string[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [isDocumentVisible, setIsDocumentVisible] = useState(
    () => document.visibilityState !== "hidden",
  );
  const lastFetchedWorkspaceId = useRef<string | null>(null);
  const inFlight = useRef<string | null>(null);

  const REFRESH_INTERVAL_MS = 30000;
  const LARGE_REFRESH_INTERVAL_MS = 60000;
  const LARGE_FILE_COUNT = 20000;
  const workspaceId = activeWorkspace?.id ?? null;
  const scopeKey = `${workspaceId}:${threadId ?? activeWorkspace?.path ?? ""}`;
  const activeScope = useRef(scopeKey);
  activeScope.current = scopeKey;
  const isEnabled = enabled;
  const isPollingEnabled = pollingEnabled ?? isEnabled;

  const refreshFiles = useCallback(async () => {
    if (!workspaceId || !isEnabled) {
      return;
    }
    if (inFlight.current === scopeKey) {
      return;
    }
    inFlight.current = scopeKey;
    const requestWorkspaceId = workspaceId;
    const requestScope = scopeKey;
    setIsLoading(true);
    onDebug?.({
      id: `${Date.now()}-client-files-list`,
      timestamp: Date.now(),
      source: "client",
      label: "files/list",
      payload: { workspaceId: requestWorkspaceId },
    });
    try {
      const response = await getWorkspaceFiles(requestWorkspaceId, threadId);
      onDebug?.({
        id: `${Date.now()}-server-files-list`,
        timestamp: Date.now(),
        source: "server",
        label: "files/list response",
        payload: response,
      });
      if (requestScope === activeScope.current) {
        const nextFiles = Array.isArray(response) ? response : [];
        setFiles((prev) => (areStringArraysEqual(prev, nextFiles) ? prev : nextFiles));
        lastFetchedWorkspaceId.current = requestScope;
      }
    } catch (error) {
      onDebug?.({
        id: `${Date.now()}-client-files-list-error`,
        timestamp: Date.now(),
        source: "error",
        label: "files/list error",
        payload: error instanceof Error ? error.message : String(error),
      });
    } finally {
      if (inFlight.current === requestScope) {
        inFlight.current = null;
        setIsLoading(false);
      }
    }
  }, [isEnabled, onDebug, workspaceId, threadId, scopeKey]);

  useEffect(() => {
    setFiles([]);
    lastFetchedWorkspaceId.current = null;
    inFlight.current = null;
  }, [scopeKey]);

  useEffect(() => {
    setIsLoading(Boolean(workspaceId && isEnabled));
  }, [isEnabled, workspaceId]);

  useEffect(() => {
    const handleVisibilityChange = () => {
      setIsDocumentVisible(document.visibilityState !== "hidden");
    };
    document.addEventListener("visibilitychange", handleVisibilityChange);
    return () => {
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    };
  }, []);

  useEffect(() => {
    if (!workspaceId || !isEnabled) {
      return;
    }
    if (lastFetchedWorkspaceId.current === scopeKey && files.length > 0) {
      return;
    }
    refreshFiles();
  }, [files.length, isEnabled, refreshFiles, workspaceId, scopeKey]);

  useEffect(() => {
    if (!workspaceId || !isPollingEnabled || !isDocumentVisible) {
      return;
    }
    const refreshInterval =
      files.length > LARGE_FILE_COUNT ? LARGE_REFRESH_INTERVAL_MS : REFRESH_INTERVAL_MS;

    const interval = window.setInterval(() => {
      // Skip if tab is hidden
      if (document.visibilityState === "hidden") {
        return;
      }
      refreshFiles().catch(() => {});
    }, refreshInterval);

    return () => {
      window.clearInterval(interval);
    };
  }, [files.length, isDocumentVisible, isPollingEnabled, refreshFiles, workspaceId]);

  const fileOptions = useMemo(() => files.filter(Boolean), [files]);

  return {
    files: fileOptions,
    isLoading,
    refreshFiles,
  };
}

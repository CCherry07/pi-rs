import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DebugEntry, WorkspaceFileListing, WorkspaceInfo } from "../../../types";
import { getWorkspaceFiles } from "../../../services/tauri";
import { workspaceFileMentions } from "../../files/workspaceFiles";

type UseWorkspaceFilesOptions = {
  activeWorkspace: WorkspaceInfo | null;
  threadId?: string | null;
  onDebug?: (entry: DebugEntry) => void;
  enabled?: boolean;
  pollingEnabled?: boolean;
};

export function useWorkspaceFiles({
  activeWorkspace,
  threadId,
  onDebug,
  enabled = true,
  pollingEnabled,
}: UseWorkspaceFilesOptions) {
  const [result, setResult] = useState<{
    scope: string;
    listing: WorkspaceFileListing | null;
    error: string | null;
  } | null>(null);
  const [pending, setPending] = useState<{ scope: string } | null>(null);
  const [isDocumentVisible, setIsDocumentVisible] = useState(
    () => document.visibilityState !== "hidden",
  );
  const lastFetchedWorkspaceId = useRef<string | null>(null);
  const inFlight = useRef<{ scope: string } | null>(null);

  const REFRESH_INTERVAL_MS = 30000;
  const LARGE_REFRESH_INTERVAL_MS = 60000;
  const LARGE_FILE_COUNT = 20000;
  const workspaceId = activeWorkspace?.id ?? null;
  const scopeKey = JSON.stringify([
    workspaceId,
    threadId ?? null,
    threadId ? null : activeWorkspace?.project ?? activeWorkspace?.path,
  ]);
  const listing = result?.scope === scopeKey ? result.listing : null;
  const error = result?.scope === scopeKey ? result.error : null;
  const fileCount = listing?.files.length ?? 0;
  const activeScope = useRef(scopeKey);
  activeScope.current = scopeKey;
  const isEnabled = enabled;
  const isLoading = isEnabled && pending?.scope === scopeKey;
  const isPollingEnabled = pollingEnabled ?? isEnabled;

  const refreshFiles = useCallback(async () => {
    if (!workspaceId || !isEnabled) {
      return;
    }
    if (inFlight.current?.scope === scopeKey) {
      return;
    }
    const request = { scope: scopeKey };
    inFlight.current = request;
    const requestWorkspaceId = workspaceId;
    const requestScope = scopeKey;
    setPending(request);
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
      if (requestScope === activeScope.current && inFlight.current === request) {
        setResult((prev) => prev?.scope === requestScope && !prev.error &&
          JSON.stringify(prev.listing) === JSON.stringify(response)
          ? prev : { scope: requestScope, listing: response, error: null });
        lastFetchedWorkspaceId.current = requestScope;
      }
    } catch (error) {
      if (requestScope === activeScope.current && inFlight.current === request) {
        setResult({ scope: requestScope, listing: null, error: String(error) });
      }
      onDebug?.({
        id: `${Date.now()}-client-files-list-error`,
        timestamp: Date.now(),
        source: "error",
        label: "files/list error",
        payload: error instanceof Error ? error.message : String(error),
      });
    } finally {
      if (inFlight.current === request) {
        inFlight.current = null;
        setPending(null);
      }
    }
  }, [isEnabled, onDebug, workspaceId, threadId, scopeKey]);

  useEffect(() => {
    setResult(null);
    lastFetchedWorkspaceId.current = null;
    inFlight.current = null;
    setPending(null);
  }, [scopeKey]);

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
    if (lastFetchedWorkspaceId.current === scopeKey) {
      return;
    }
    refreshFiles();
  }, [isEnabled, refreshFiles, workspaceId, scopeKey]);

  useEffect(() => {
    if (!workspaceId || !isPollingEnabled || !isDocumentVisible) {
      return;
    }
    const refreshInterval =
      fileCount > LARGE_FILE_COUNT ? LARGE_REFRESH_INTERVAL_MS : REFRESH_INTERVAL_MS;

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
  }, [fileCount, isDocumentVisible, isPollingEnabled, refreshFiles, workspaceId]);

  const fileOptions = useMemo(() => listing ? workspaceFileMentions(listing) : [], [listing]);

  return {
    files: fileOptions,
    listing,
    error,
    isLoading,
    refreshFiles,
  };
}

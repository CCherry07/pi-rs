import { useCallback, useEffect, useRef, useState } from "react";
import type { DebugEntry } from "@/types";
import { prepareThread, reloadThread } from "@services/tauri";
import { commandsFromThread, type RuntimeCommand } from "@utils/desktopCommands";

export function useWorkspaceDraft(
  workspaceId: string | null,
  activeThreadId: string | null,
  onDebug?: (entry: DebugEntry) => void,
) {
  const [draft, setDraft] = useState<{ workspaceId: string; commands: RuntimeCommand[] } | null>(null);
  const request = useRef(0);
  const reloadRequest = useRef(0);
  useEffect(() => {
    const id = ++request.current;
    setDraft(null);
    if (!workspaceId || activeThreadId) return;
    let disposed = false;
    void prepareThread(workspaceId).then(({ thread }) => {
      if (!disposed && request.current === id) setDraft({ workspaceId, commands: commandsFromThread(thread) });
    }).catch((error: unknown) => {
      if (disposed || request.current !== id) return;
      setDraft(null);
      onDebug?.({
        id: `${Date.now()}-workspace-draft-error`, timestamp: Date.now(),
        source: "error", label: "workspace/draft preparation failed",
        payload: error instanceof Error ? error.message : String(error),
      });
    });
    const invalidate = () => { ++request.current; };
    return () => { disposed = true; invalidate(); };
  }, [workspaceId, activeThreadId, onDebug]);
  const reload = useCallback(async () => {
    if (!workspaceId || activeThreadId) return;
    const id = request.current;
    const reloadId = ++reloadRequest.current;
    try {
      const { thread } = await reloadThread(workspaceId);
      if (request.current !== id || reloadRequest.current !== reloadId) return;
      // Only a successful replacement supersedes a pending initial catalog.
      // Failed reloads must leave that still-valid generation available.
      ++request.current;
      setDraft({ workspaceId, commands: commandsFromThread(thread) });
    } catch (error) {
      onDebug?.({
        id: `${Date.now()}-workspace-draft-reload-error`, timestamp: Date.now(),
        source: "error", label: "workspace/draft reload failed",
        payload: error instanceof Error ? error.message : String(error),
      });
      throw error;
    }
  }, [activeThreadId, onDebug, workspaceId]);

  return {
    commands: !activeThreadId && draft?.workspaceId === workspaceId ? draft.commands : [],
    reload,
  };
}

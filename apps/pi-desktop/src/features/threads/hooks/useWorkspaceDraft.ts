import { useCallback, useEffect, useState } from "react";
import type { DebugEntry } from "@/types";
import { prepareThread, reloadThread } from "@services/tauri";
import { commandsFromThread, type RuntimeCommand } from "@utils/desktopCommands";

export function useWorkspaceDraft(
  workspaceId: string | null,
  activeThreadId: string | null,
  onDebug?: (entry: DebugEntry) => void,
) {
  const [draft, setDraft] = useState<{ workspaceId: string; commands: RuntimeCommand[] } | null>(null);
  useEffect(() => {
    setDraft(null);
    if (!workspaceId || activeThreadId) return;
    let disposed = false;
    void prepareThread(workspaceId).then(({ thread }) => {
      if (!disposed) setDraft({ workspaceId, commands: commandsFromThread(thread) });
    }).catch((error: unknown) => {
      if (disposed) return;
      setDraft(null);
      onDebug?.({
        id: `${Date.now()}-workspace-draft-error`, timestamp: Date.now(),
        source: "error", label: "workspace/draft preparation failed",
        payload: error instanceof Error ? error.message : String(error),
      });
    });
    return () => { disposed = true; };
  }, [workspaceId, activeThreadId, onDebug]);
  const reload = useCallback(async () => {
    if (!workspaceId || activeThreadId) return;
    try {
      const { thread } = await reloadThread(workspaceId);
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

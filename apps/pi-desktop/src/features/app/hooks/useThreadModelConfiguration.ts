import { useEffect } from "react";
import { configureThread } from "@services/tauri";
import type { DebugEntry } from "@/types";

type Options = {
  workspaceId: string | null;
  threadId: string | null;
  model: string | null;
  effort: string | null;
  isCatalogCurrent: (workspaceId: string, threadId: string) => boolean;
  isSessionLoading?: boolean;
  onDebug?: (entry: DebugEntry) => void;
};

export function useThreadModelConfiguration({
  workspaceId,
  threadId,
  model,
  effort,
  isCatalogCurrent,
  isSessionLoading = false,
  onDebug,
}: Options) {
  useEffect(() => {
    if (
      !workspaceId ||
      !threadId ||
      isSessionLoading ||
      !isCatalogCurrent(workspaceId, threadId) ||
      (!model && !effort)
    )
      return;
    void configureThread(workspaceId, threadId, { model, effort }).catch(
      (error) => {
        onDebug?.({
          id: `${Date.now()}-pi-thread-configuration-error`,
          timestamp: Date.now(),
          source: "error",
          label: "pi/thread configuration error",
          payload: error instanceof Error ? error.message : String(error),
        });
      },
    );
  }, [
    workspaceId,
    threadId,
    model,
    effort,
    isCatalogCurrent,
    isSessionLoading,
    onDebug,
  ]);
}

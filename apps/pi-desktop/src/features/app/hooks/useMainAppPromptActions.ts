import { useCallback } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import type { SendMessageResult, WorkspaceInfo } from "@/types";

type PromptPayload = {
  scope: "workspace" | "global";
  name: string;
  description?: string | null;
  argumentHint?: string | null;
  content: string;
};

type PromptUpdatePayload = {
  path: string;
  name: string;
  description?: string | null;
  argumentHint?: string | null;
  content: string;
};

type UseMainAppPromptActionsArgs = {
  activeWorkspace: WorkspaceInfo | null;
  startThreadForWorkspace: (
    workspaceId: string,
    options?: { activate?: boolean },
  ) => Promise<string | null>;
  sendUserMessageToThread: (
    workspace: WorkspaceInfo,
    threadId: string,
    text: string,
    images?: string[],
  ) => Promise<void | SendMessageResult>;
  createPrompt: (data: PromptPayload) => Promise<void>;
  updatePrompt: (data: PromptUpdatePayload) => Promise<void>;
  deletePrompt: (path: string) => Promise<void>;
  movePrompt: (data: { path: string; scope: "workspace" | "global" }) => Promise<void>;
  getWorkspacePromptsDir: () => Promise<string>;
  getGlobalPromptsDir: () => Promise<string | null>;
  alertError: (error: unknown) => void;
};

export function useMainAppPromptActions({
  activeWorkspace,
  startThreadForWorkspace,
  sendUserMessageToThread,
  createPrompt,
  updatePrompt,
  deletePrompt,
  movePrompt,
  getWorkspacePromptsDir,
  getGlobalPromptsDir,
  alertError,
}: UseMainAppPromptActionsArgs) {
  const runPromptAction = useCallback(
    async (action: () => Promise<void>) => {
      try {
        await action();
      } catch (error) {
        alertError(error);
      }
    },
    [alertError],
  );

  const handleCreatePrompt = useCallback(
    async (data: PromptPayload) => {
      await runPromptAction(() => createPrompt(data));
    },
    [createPrompt, runPromptAction],
  );

  const handleUpdatePrompt = useCallback(
    async (data: PromptUpdatePayload) => {
      await runPromptAction(() => updatePrompt(data));
    },
    [runPromptAction, updatePrompt],
  );

  const handleDeletePrompt = useCallback(
    async (path: string) => {
      await runPromptAction(() => deletePrompt(path));
    },
    [deletePrompt, runPromptAction],
  );

  const handleMovePrompt = useCallback(
    async (data: { path: string; scope: "workspace" | "global" }) => {
      await runPromptAction(() => movePrompt(data));
    },
    [movePrompt, runPromptAction],
  );

  const handleRevealWorkspacePrompts = useCallback(async () => {
    await runPromptAction(async () => {
      const path = await getWorkspacePromptsDir();
      await revealItemInDir(path);
    });
  }, [getWorkspacePromptsDir, runPromptAction]);

  const handleRevealGeneralPrompts = useCallback(async () => {
    await runPromptAction(async () => {
      const path = await getGlobalPromptsDir();
      if (!path) {
        return;
      }
      await revealItemInDir(path);
    });
  }, [getGlobalPromptsDir, runPromptAction]);

  const handleSendPromptToNewAgent = useCallback(
    async (text: string) => {
      const trimmed = text.trim();
      if (!activeWorkspace || !trimmed) {
        return;
      }
      const threadId = await startThreadForWorkspace(activeWorkspace.id, {
        activate: false,
      });
      if (!threadId) {
        return;
      }
      await sendUserMessageToThread(activeWorkspace, threadId, trimmed, []);
    },
    [activeWorkspace, sendUserMessageToThread, startThreadForWorkspace],
  );

  return {
    handleCreatePrompt,
    handleUpdatePrompt,
    handleDeletePrompt,
    handleMovePrompt,
    handleRevealWorkspacePrompts,
    handleRevealGeneralPrompts,
    handleSendPromptToNewAgent,
  };
}

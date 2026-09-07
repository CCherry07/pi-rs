import { useMemo } from "react";
import { useTauriEvent } from "../../app/hooks/useTauriEvent";
import {
  subscribeMenuCycleModel,
  subscribeMenuCycleReasoning,
} from "../../../services/events";

type ModelOption = { id: string; displayName: string; model: string };

type UseComposerMenuActionsOptions = {
  models: ModelOption[];
  selectedModelId: string | null;
  onSelectModel: (id: string) => void;
  reasoningOptions: string[];
  selectedEffort: string | null;
  onSelectEffort: (effort: string) => void;
  reasoningSupported: boolean;
  onFocusComposer?: () => void;
};

export function useComposerMenuActions({
  models,
  selectedModelId,
  onSelectModel,
  reasoningOptions,
  selectedEffort,
  onSelectEffort,
  reasoningSupported,
  onFocusComposer,
}: UseComposerMenuActionsOptions) {
  const handlers = useMemo(
    () => ({
      cycleModel() {
        if (models.length === 0) {
          return;
        }
        const currentIndex = models.findIndex((model) => model.id === selectedModelId);
        const nextIndex = currentIndex >= 0 ? (currentIndex + 1) % models.length : 0;
        const nextModel = models[nextIndex];
        if (nextModel) {
          onFocusComposer?.();
          onSelectModel(nextModel.id);
        }
      },
      cycleReasoning() {
        if (!reasoningSupported || reasoningOptions.length === 0) {
          return;
        }
        const currentIndex = reasoningOptions.indexOf(selectedEffort ?? "");
        const nextIndex =
          currentIndex >= 0 ? (currentIndex + 1) % reasoningOptions.length : 0;
        const nextEffort = reasoningOptions[nextIndex];
        if (nextEffort) {
          onFocusComposer?.();
          onSelectEffort(nextEffort);
        }
      },
    }),
    [
      models,
      onFocusComposer,
      onSelectEffort,
      onSelectModel,
      reasoningOptions,
      reasoningSupported,
      selectedEffort,
      selectedModelId,
    ],
  );

  useTauriEvent(subscribeMenuCycleModel, () => {
    handlers.cycleModel();
  });

  useTauriEvent(subscribeMenuCycleReasoning, () => {
    handlers.cycleReasoning();
  });

  return handlers;
}

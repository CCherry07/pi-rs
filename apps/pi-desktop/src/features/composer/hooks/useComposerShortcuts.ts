import { useEffect } from "react";
import { matchesShortcut } from "../../../utils/shortcuts";

type ModelOption = { id: string; displayName: string; model: string };

type UseComposerShortcutsOptions = {
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  modelShortcut: string | null;
  reasoningShortcut: string | null;
  models: ModelOption[];
  selectedModelId: string | null;
  onSelectModel: (id: string) => void;
  reasoningOptions: string[];
  selectedEffort: string | null;
  onSelectEffort: (effort: string) => void;
  reasoningSupported: boolean;
};

export function useComposerShortcuts({
  textareaRef,
  modelShortcut,
  reasoningShortcut,
  models,
  selectedModelId,
  onSelectModel,
  reasoningOptions,
  selectedEffort,
  onSelectEffort,
  reasoningSupported,
}: UseComposerShortcutsOptions) {
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.repeat) {
        return;
      }
      if (document.activeElement !== textareaRef.current) {
        return;
      }
      if (matchesShortcut(event, modelShortcut)) {
        event.preventDefault();
        if (models.length === 0) {
          return;
        }
        const currentIndex = models.findIndex((model) => model.id === selectedModelId);
        const nextIndex = currentIndex >= 0 ? (currentIndex + 1) % models.length : 0;
        const nextModel = models[nextIndex];
        if (nextModel) {
          onSelectModel(nextModel.id);
        }
        return;
      }
      if (matchesShortcut(event, reasoningShortcut)) {
        event.preventDefault();
        if (!reasoningSupported || reasoningOptions.length === 0) {
          return;
        }
        const currentIndex = reasoningOptions.indexOf(selectedEffort ?? "");
        const nextIndex =
          currentIndex >= 0 ? (currentIndex + 1) % reasoningOptions.length : 0;
        const nextEffort = reasoningOptions[nextIndex];
        if (nextEffort) {
          onSelectEffort(nextEffort);
        }
        return;
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [
    modelShortcut,
    models,
    onSelectEffort,
    onSelectModel,
    reasoningOptions,
    reasoningShortcut,
    reasoningSupported,
    selectedEffort,
    selectedModelId,
    textareaRef,
  ]);
}

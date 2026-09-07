import i18n from "@/i18n";
import type { ModelOption } from "../../../types";
import type { WorkspaceHomeRunInstance } from "../hooks/useWorkspaceHome";

export const INSTANCE_OPTIONS = [1, 2, 3, 4];

export const CARET_ANCHOR_GAP = 8;

export const buildIconPath = (workspacePath: string) => {
  const separator = workspacePath.includes("\\") ? "\\" : "/";
  return `${workspacePath.replace(/[\\/]+$/, "")}${separator}icon.png`;
};

export const resolveModelLabel = (model: ModelOption | null) =>
  model?.displayName?.trim() ||
  model?.model?.trim() ||
  i18n.t("home.defaultModel", { ns: "workspaces" });

export const buildLabelCounts = (instances: WorkspaceHomeRunInstance[]) => {
  const counts = new Map<string, number>();
  instances.forEach((instance) => {
    counts.set(instance.modelLabel, (counts.get(instance.modelLabel) ?? 0) + 1);
  });
  return counts;
};

export const buildModelSummary = (
  models: ModelOption[],
  modelSelections: Record<string, number>,
) => {
  const totalInstances = Object.values(modelSelections).reduce(
    (sum, count) => sum + count,
    0,
  );
  const selectedModels = models.filter((model) => modelSelections[model.id]);
  if (selectedModels.length === 0) {
    return i18n.t("home.selectModels", { ns: "workspaces" });
  }
  if (selectedModels.length === 1) {
    const model = selectedModels[0];
    const count = modelSelections[model.id] ?? 1;
    return `${resolveModelLabel(model)} · ${count}x`;
  }
  return i18n.t("home.modelSummary", {
    ns: "workspaces",
    count: selectedModels.length,
    runs: totalInstances,
  });
};

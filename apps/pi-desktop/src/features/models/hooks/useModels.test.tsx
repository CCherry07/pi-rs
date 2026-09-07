// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "../../../types";
import { getModelList } from "../../../services/tauri";
import { useModels } from "./useModels";

vi.mock("../../../services/tauri", () => ({
  getModelList: vi.fn(),
}));

const workspace: WorkspaceInfo = {
  id: "workspace-1",
  name: "Workspace One",
  path: "/tmp/workspace-one",
  settings: { sidebarCollapsed: false },
};

afterEach(() => {
  vi.clearAllMocks();
});

describe("useModels", () => {
  it("selects a compatible thinking level when the model changes", async () => {
    vi.mocked(getModelList).mockResolvedValue({
      data: [
        {
          id: "provider/reasoning",
          model: "reasoning",
          displayName: "Reasoning",
          supportedReasoningEfforts: [
            { reasoningEffort: "off", description: "Off" },
            { reasoningEffort: "high", description: "High" },
          ],
          defaultReasoningEffort: "high",
          isDefault: true,
        },
        {
          id: "provider/plain",
          model: "plain",
          displayName: "Plain",
          supportedReasoningEfforts: [
            { reasoningEffort: "off", description: "Off" },
          ],
          defaultReasoningEffort: "off",
          isDefault: false,
        },
      ],
    });

    const { result } = renderHook(() => useModels({ activeWorkspace: workspace }));

    await waitFor(() => {
      expect(result.current.selectedModelId).toBe("provider/reasoning");
      expect(result.current.selectedEffort).toBe("high");
    });

    act(() => {
      result.current.setSelectedEffort("high");
      result.current.setSelectedModelId("provider/plain");
    });

    await waitFor(() => {
      expect(result.current.selectedModelId).toBe("provider/plain");
      expect(result.current.selectedEffort).toBe("off");
    });
  });
});

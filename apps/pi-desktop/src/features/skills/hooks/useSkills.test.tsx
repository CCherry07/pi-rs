// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "../../../types";
import { getSkillsList } from "../../../services/tauri";
import { useSkills } from "./useSkills";

vi.mock("../../../services/tauri", () => ({
  getSkillsList: vi.fn(),
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

describe("useSkills", () => {
  it("loads and explicitly refreshes the active workspace catalog", async () => {
    vi.mocked(getSkillsList)
      .mockResolvedValueOnce({
        result: { skills: [{ name: "first", path: "/skills/first" }] },
      })
      .mockResolvedValueOnce({
        result: {
          skills: [
            { name: "first", path: "/skills/first" },
            { name: "second", path: "/skills/second" },
          ],
        },
      });

    const { result } = renderHook(() => useSkills({ activeWorkspace: workspace }));

    await waitFor(() => {
      expect(result.current.skills.map((skill) => skill.name)).toEqual(["first"]);
    });

    await act(async () => {
      await result.current.refreshSkills();
    });

    expect(getSkillsList).toHaveBeenCalledTimes(2);
    expect(result.current.skills.map((skill) => skill.name)).toEqual([
      "first",
      "second",
    ]);
  });
});

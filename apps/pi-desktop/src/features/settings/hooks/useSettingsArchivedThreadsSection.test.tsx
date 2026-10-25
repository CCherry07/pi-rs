// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "@/types";
import { listThreads, type ThreadListPage } from "@services/tauri";
import { useSettingsArchivedThreadsSection } from "./useSettingsArchivedThreadsSection";

vi.mock("@services/tauri", () => ({
  listThreads: vi.fn(),
  deleteThread: vi.fn(),
  unarchiveThread: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn() }));

const projects: WorkspaceInfo[] = ["one", "two"].map((id) => ({
  id,
  name: id,
  path: `/projects/${id}`,
  settings: { sidebarCollapsed: false },
}));
const page = (
  workspaceId: string,
  ids: string[],
  nextCursor: string | null = null,
): ThreadListPage => ({
  workspaceId,
  data: ids.map((id) => ({ id, preview: id, createdAt: 1, updatedAt: 2 })),
  nextCursor,
});

beforeEach(() => vi.resetAllMocks());
afterEach(cleanup);

describe("archived session pagination", () => {
  it("batches projects and follows every remaining archive cursor", async () => {
    vi.mocked(listThreads)
      .mockResolvedValueOnce([
        page(
          "one",
          Array.from({ length: 200 }, (_, index) => `first-${index}`),
          "archive-cursor",
        ),
        page("two", ["other-project"]),
      ])
      .mockResolvedValueOnce([page("one", ["last-session"])]);
    const { result } = renderHook(() =>
      useSettingsArchivedThreadsSection({ enabled: true, projects }),
    );

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.archivedThreads).toHaveLength(202);
    expect(result.current.error).toBeNull();
    expect(listThreads).toHaveBeenNthCalledWith(
      1,
      [{ workspaceId: "one" }, { workspaceId: "two" }],
      { limit: 200, sortKey: "updated_at", archived: true },
    );
    expect(listThreads).toHaveBeenNthCalledWith(
      2,
      [{ workspaceId: "one", cursor: "archive-cursor" }],
      { limit: 200, sortKey: "updated_at", archived: true },
    );
    expect(
      result.current.archivedThreads.find(
        (entry) => entry.id === "other-project",
      )?.workspaceId,
    ).toBe("two");
  });

  it("keeps loaded entries visible and reports a later page failure", async () => {
    vi.mocked(listThreads)
      .mockResolvedValueOnce([
        page("one", ["retained"], "next"),
        page("two", []),
      ])
      .mockRejectedValueOnce(new Error("archive read failed"));
    const { result } = renderHook(() =>
      useSettingsArchivedThreadsSection({ enabled: true, projects }),
    );

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.archivedThreads.map((entry) => entry.id)).toEqual([
      "retained",
    ]);
    expect(result.current.error).toContain("archive read failed");
  });

  it("discards a superseded archive refresh before requesting its next page", async () => {
    let resolve!: (pages: ThreadListPage[]) => void;
    vi.mocked(listThreads)
      .mockReturnValueOnce(
        new Promise((complete) => {
          resolve = complete;
        }),
      )
      .mockResolvedValueOnce([page("one", ["fresh"]), page("two", [])]);
    const { result } = renderHook(() =>
      useSettingsArchivedThreadsSection({ enabled: true, projects }),
    );

    act(() => result.current.onRefresh());
    await waitFor(() =>
      expect(result.current.archivedThreads.map((entry) => entry.id)).toEqual([
        "fresh",
      ]),
    );
    await act(async () => {
      resolve([page("one", ["stale"], "stale-cursor")]);
    });
    expect(result.current.archivedThreads.map((entry) => entry.id)).toEqual([
      "fresh",
    ]);
    expect(listThreads).toHaveBeenCalledTimes(2);
    expect(result.current.loading).toBe(false);
  });
});

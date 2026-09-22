// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { listManagedWorktrees } from "../../../services/tauri";
import { Home } from "./Home";

vi.mock("../../../services/tauri", () => ({ listManagedWorktrees: vi.fn(), removeWorktree: vi.fn(), confirmWorktreeDiscard: vi.fn() }));
afterEach(cleanup);

it("exposes orphaned worktree recovery from Home without a project and refreshes failures", async () => {
  vi.mocked(listManagedWorktrees).mockResolvedValueOnce([]).mockResolvedValueOnce([
    { id: "orphan", name: "Orphaned worktree", status: "cleanupRequired", errors: ["Parent project was removed"], memberCount: 2 },
  ]);
  render(<Home onAddWorkspace={vi.fn()} onAddWorkspaceFromUrl={vi.fn()} latestAgentRuns={[]} isLoadingLatestAgents={false} onSelectThread={vi.fn()} />);
  await act(async () => { await Promise.resolve(); });
  expect(screen.queryByRole("region", { name: "Unfinished worktree operations" })).toBeNull();
  await act(async () => { window.dispatchEvent(new Event("pi-worktree-groups-changed")); });
  expect(await screen.findByText("Orphaned worktree · 2 repositories")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Retry cleanup" })).toBeTruthy();
});

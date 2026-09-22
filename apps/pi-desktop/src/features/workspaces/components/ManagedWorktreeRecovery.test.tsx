// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { confirmWorktreeDiscard, listManagedWorktrees, removeWorktree } from "../../../services/tauri";
import { ManagedWorktreeRecovery } from "./ManagedWorktreeRecovery";

vi.mock("../../../services/tauri", () => ({ confirmWorktreeDiscard: vi.fn(), listManagedWorktrees: vi.fn(), removeWorktree: vi.fn() }));
afterEach(cleanup);
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(listManagedWorktrees).mockResolvedValue([
    { id: "partial", name: "Incomplete feature", status: "cleanupRequired", errors: ["Repository is unavailable"], memberCount: 2 },
    { id: "ready", name: "Working feature", status: "ready", errors: [], memberCount: 1 },
  ]);
});

it("offers ordinary cleanup for unfinished groups and retains failures for retry", async () => {
  vi.mocked(removeWorktree).mockRejectedValueOnce(new Error("Worktree has uncommitted changes")).mockResolvedValueOnce();
  render(<ManagedWorktreeRecovery />);
  expect(await screen.findByText("Incomplete feature · 2 repositories")).toBeTruthy();
  expect(screen.queryByText("Working feature")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Retry cleanup" }));
  expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Error: Worktree has uncommitted changes");
  expect(removeWorktree).toHaveBeenCalledWith("partial");
  expect(confirmWorktreeDiscard).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Retry cleanup" }));
  await waitFor(() => { expect(screen.queryByText("Incomplete feature · 2 repositories")).toBeNull(); });
});

it("shows ready groups with deletion errors and requires explicit confirmation before force cleanup", async () => {
  vi.mocked(listManagedWorktrees).mockResolvedValue([
    { id: "dirty", name: "Dirty feature", status: "ready", errors: ["Worktree has local files"], memberCount: 2 },
  ]);
  vi.mocked(confirmWorktreeDiscard).mockResolvedValueOnce(false).mockResolvedValueOnce(true);
  vi.mocked(removeWorktree).mockResolvedValue();
  render(<ManagedWorktreeRecovery />);
  const discard = await screen.findByRole("button", { name: "Discard changes and clean up" });
  fireEvent.click(discard);
  await waitFor(() => { expect(confirmWorktreeDiscard).toHaveBeenCalledWith("Dirty feature"); expect(discard).toHaveProperty("disabled", false); });
  expect(removeWorktree).not.toHaveBeenCalled();
  fireEvent.click(discard);
  await waitFor(() => { expect(removeWorktree).toHaveBeenCalledWith("dirty", true); });
});

// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { GitDiffPanel } from "./GitDiffPanel";

afterEach(cleanup);
it("shows the group delivery action across Git modes only when the owner enables it", () => {
  const props = {
    mode: "log" as const, onModeChange: vi.fn(), filePanelMode: "git" as const, onFilePanelModeChange: vi.fn(),
    branchName: "feature", totalAdditions: 0, totalDeletions: 0, fileStatus: "clean", stagedFiles: [], unstagedFiles: [], logEntries: [],
  };
  const onOpenWorktreeDelivery = vi.fn();
  const { rerender } = render(<GitDiffPanel {...props} />);
  expect(screen.queryByRole("button", { name: "Preview worktree delivery" })).toBeNull();
  rerender(<GitDiffPanel {...props} onOpenWorktreeDelivery={onOpenWorktreeDelivery} />);
  fireEvent.click(screen.getByRole("button", { name: "Preview worktree delivery" }));
  expect(onOpenWorktreeDelivery).toHaveBeenCalledTimes(1);
  rerender(<GitDiffPanel {...props} mode="diff" onOpenWorktreeDelivery={onOpenWorktreeDelivery} />);
  expect(screen.getByRole("button", { name: "Preview worktree delivery" })).toBeTruthy();
});

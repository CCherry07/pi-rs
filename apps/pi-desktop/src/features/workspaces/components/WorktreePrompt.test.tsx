// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { WorktreePrompt } from "./WorktreePrompt";
import type { GitInventory } from "../../git/gitContext";
import type { WorktreePlanPreview } from "../../../services/tauri";

vi.mock("../../../services/tauri", () => ({
  listManagedWorktrees: vi.fn().mockResolvedValue([]),
  removeWorktree: vi.fn(),
}));

const scrollIntoView = vi.fn();
Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollIntoView });

afterEach(() => {
  cleanup();
  scrollIntoView.mockClear();
});

const baseProps = {
  workspaceName: "Repo",
  name: "",
  branch: "feature/new-worktree",
  copyAgentsMd: false,
  setupScript: "",
  onNameChange: vi.fn(),
  onChange: vi.fn(),
  onCopyAgentsMdChange: vi.fn(),
  onSetupScriptChange: vi.fn(),
  onCancel: vi.fn(),
  onConfirm: vi.fn(),
};

describe("WorktreePrompt", () => {
  it("reviews the selected repositories before creating and displays every root mapping", () => {
    const onReview = vi.fn();
    const onConfirm = vi.fn();
    const onCheckoutChange = vi.fn();
    const onExecutionRootChange = vi.fn();
    const inventory: GitInventory = {
      workspace: {
        roots: [
          { id: "app", name: "App", path: "/repo/app/src", ownership: { kind: "external" } },
          { id: "docs", name: "Docs", path: "/repo/docs", ownership: { kind: "external" } },
        ], primaryRoot: "app", executionDir: "/repo/app/src",
      },
      checkouts: [
        { key: "app", workdir: "/repo/app", gitDir: "/repo/app/.git", commonDir: "/repo/app/.git", rootIds: ["app"] },
        { key: "docs", workdir: "/repo/docs", gitDir: "/repo/docs/.git", commonDir: "/repo/docs/.git", rootIds: ["docs"] },
      ], defaultCheckoutKey: "app", directoryRootIds: [], errors: [],
    };
    const checkouts = inventory.checkouts.map((checkout) => ({
      checkout, selected: true, branch: "pi/feature", branchWasEdited: false, startPoint: "HEAD", branches: [],
    }));
    const props = { ...baseProps, inventory, checkouts, onReview, onConfirm, onCheckoutChange, onExecutionRootChange };
    const { rerender } = render(<WorktreePrompt {...props} plan={null} />);
    fireEvent.change(screen.getByLabelText("Branch for /repo/docs"), { target: { value: "docs/feature" } });
    expect(onCheckoutChange).toHaveBeenCalledWith("docs", { branch: "docs/feature" });
    fireEvent.change(screen.getByLabelText("Start from ref for /repo/app"), { target: { value: "release" } });
    expect(onCheckoutChange).toHaveBeenCalledWith("app", { startPoint: "release" });
    fireEvent.click(screen.getByRole("checkbox", { name: "/repo/docs" }));
    expect(onCheckoutChange).toHaveBeenCalledWith("docs", { selected: false });
    fireEvent.change(screen.getByLabelText("Working directory"), { target: { value: "docs" } });
    expect(onExecutionRootChange).toHaveBeenCalledWith("docs");
    fireEvent.click(screen.getByRole("button", { name: "Review mapping" }));
    expect(onReview).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
    const plan: WorktreePlanPreview = {
      id: "prepared", parentId: "parent", name: "Feature", source: inventory.workspace,
      workspace: { ...inventory.workspace, roots: [
        { ...inventory.workspace.roots[0], path: "/worktrees/feature/app/src", ownership: { kind: "managedWorktree", sourceRoot: "app" } },
        inventory.workspace.roots[1],
      ], executionDir: "/worktrees/feature/app/src" },
      checkouts: [{ sourceWorkdir: "/repo/app", destination: "/worktrees/feature/app", branch: "pi/feature", startOid: "abc123", rootIds: ["app"] }], warnings: [],
    };
    rerender(<WorktreePrompt {...props} plan={plan} />);
    const mappingPreview = screen.getByRole("region", { name: "Root mapping preview" });
    expect(document.activeElement).toBe(mappingPreview);
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "start" });
    expect(screen.getByText("→ /worktrees/feature/app/src")).toBeTruthy();
    expect(screen.getByText("Retained at its current path")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("guards backdrop cancel while busy", () => {
    const onCancel = vi.fn();
    const { container, rerender } = render(
      <WorktreePrompt {...baseProps} onCancel={onCancel} isBusy />,
    );

    let backdrop = container.querySelector(".ds-modal-backdrop");
    expect(backdrop).toBeTruthy();
    if (!backdrop) {
      throw new Error("Expected worktree prompt backdrop");
    }
    fireEvent.click(backdrop);
    expect(onCancel).not.toHaveBeenCalled();

    rerender(<WorktreePrompt {...baseProps} onCancel={onCancel} isBusy={false} />);
    backdrop = container.querySelector(".ds-modal-backdrop");
    if (!backdrop) {
      throw new Error("Expected worktree prompt backdrop");
    }
    fireEvent.click(backdrop);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("handles Escape and Enter on branch input", () => {
    const onCancel = vi.fn();
    const onConfirm = vi.fn();
    render(
      <WorktreePrompt
        {...baseProps}
        onCancel={onCancel}
        onConfirm={onConfirm}
        isBusy={false}
        branchSuggestions={[]}
      />,
    );

    const branchInput = screen.getByLabelText("Branch name");
    fireEvent.keyDown(branchInput, {
      key: "Escape",
      code: "Escape",
      keyCode: 27,
      which: 27,
    });
    fireEvent.keyDown(branchInput, { key: "Enter" });

    return waitFor(() => {
      expect(onCancel).toHaveBeenCalled();
      expect(onConfirm).toHaveBeenCalled();
    });
  });
});

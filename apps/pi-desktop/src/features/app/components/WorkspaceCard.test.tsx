// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "../../../types";
import { WorkspaceCard } from "./WorkspaceCard";
import { WorktreeCard } from "./WorktreeCard";

const workspace: WorkspaceInfo = {
  id: "app", name: "Desktop project", path: "/projects/app",
  settings: { sidebarCollapsed: false },
  project: { id: "project", name: "Desktop project", primaryRoot: "app", roots: [
    { id: "app", name: "App", path: "/projects/app", ownership: { kind: "external" } },
    { id: "docs", name: "Docs", path: "/projects/docs", ownership: { kind: "external" } },
  ] },
};
const select = vi.fn();
const edit = vi.fn();
const collapse = vi.fn();
const actions = [{ id: "edit", label: "Edit workspace…", onSelect: edit }];

function Card({ value = workspace }: { value?: WorkspaceInfo }) {
  return <WorkspaceCard workspace={value} summary="3 conversations · Updated 2m"
    isActive isCollapsed={false} actions={actions} onSelectWorkspace={select}
    onShowWorkspaceMenu={vi.fn()} onToggleWorkspaceCollapse={collapse} />;
}

beforeEach(() => { vi.useFakeTimers(); vi.clearAllMocks(); });
afterEach(() => { cleanup(); vi.useRealTimers(); });

it("keeps rows compact and shows full details after hover, including across the panel gap", () => {
  render(<Card />);
  expect(screen.queryByText("3 conversations · Updated 2m")).toBeNull();
  expect(screen.queryByText("/projects/docs")).toBeNull();
  const row = screen.getByText(workspace.name).closest(".sidebar-hover-anchor")!;
  fireEvent.mouseEnter(row);
  act(() => { vi.advanceTimersByTime(250); });
  expect(screen.queryByRole("dialog")).toBeNull();
  act(() => { vi.advanceTimersByTime(50); });
  const panel = screen.getByRole("dialog", { name: workspace.name });
  expect(panel.textContent).toContain("3 conversations · Updated 2m");
  expect(within(panel).getByText("/projects/docs")).toBeTruthy();
  fireEvent.mouseLeave(row);
  act(() => { vi.advanceTimersByTime(100); });
  fireEvent.mouseEnter(panel);
  act(() => { vi.advanceTimersByTime(300); });
  expect(screen.getByRole("dialog")).toBe(panel);
  fireEvent.mouseLeave(panel);
  act(() => { vi.advanceTimersByTime(200); });
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("opens actions by click and keyboard without selecting the project", () => {
  render(<Card />);
  const trigger = screen.getByRole("button", { name: "Details and actions" });
  fireEvent.keyDown(trigger, { key: "Enter" });
  fireEvent.click(trigger);
  const action = screen.getByRole("button", { name: "Edit workspace…" });
  expect(document.activeElement).toBe(action);
  fireEvent.keyDown(action, { key: "Enter" });
  fireEvent.click(action);
  expect(edit).toHaveBeenCalledOnce();
  expect(select).not.toHaveBeenCalled();
  expect(screen.queryByRole("dialog")).toBeNull();

  fireEvent.keyDown(trigger, { key: "ArrowRight" });
  fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(trigger);
  const toggle = screen.getByRole("button", { name: "Hide agents" });
  fireEvent.keyDown(toggle, { key: " " });
  fireEvent.click(toggle);
  expect(collapse).toHaveBeenCalledExactlyOnceWith(workspace.id, true);
  expect(select).not.toHaveBeenCalled();
});

it("dismisses on outside interaction or sidebar scroll while allowing panel scrolling", () => {
  render(<Card />);
  const trigger = screen.getByRole("button", { name: "Details and actions" });
  fireEvent.click(trigger);
  fireEvent.scroll(screen.getByRole("dialog"));
  expect(screen.getByRole("dialog")).toBeTruthy();
  fireEvent.pointerDown(document.body);
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.click(trigger);
  fireEvent.scroll(window);
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("cancels a pending hover when another project opens immediately", () => {
  render(<><Card /><Card value={{ ...workspace, id: "other", name: "Other project" }} /></>);
  fireEvent.mouseEnter(screen.getByText(workspace.name).closest(".sidebar-hover-anchor")!);
  const other = screen.getByText("Other project").closest(".workspace-row")!;
  fireEvent.keyDown(other, { key: "ArrowRight" });
  act(() => { vi.advanceTimersByTime(500); });
  expect(screen.queryByRole("dialog", { name: workspace.name })).toBeNull();
  expect(screen.getByRole("dialog", { name: "Other project" })).toBeTruthy();
});

it("shows a worktree checkout path and clears hover state when deletion starts", () => {
  const worktree: WorkspaceInfo = { ...workspace, id: "tree", kind: "worktree", name: "Feature",
    path: "/worktrees/feature/app", worktree: { branch: "feature", managed: true, checkoutCount: 2 } };
  const card = (deleting: boolean) => <WorktreeCard worktree={worktree} isActive={false}
    isDeleting={deleting} actions={actions} onSelectWorkspace={select}
    onShowWorktreeMenu={vi.fn()} onToggleWorkspaceCollapse={collapse} />;
  const { rerender } = render(card(false));
  fireEvent.click(screen.getByRole("button", { name: "Details and actions" }));
  expect(screen.getByText("/worktrees/feature/app")).toBeTruthy();
  expect(screen.queryByText("/projects/docs")).toBeNull();
  rerender(card(true));
  expect(screen.queryByRole("dialog")).toBeNull();
  const row = screen.getByText("Feature").closest(".worktree-row")!;
  fireEvent.click(row);
  fireEvent.keyDown(row, { key: "ArrowRight" });
  expect(select).not.toHaveBeenCalled();
  rerender(card(false));
  act(() => { vi.advanceTimersByTime(500); });
  expect(screen.queryByRole("dialog")).toBeNull();
});

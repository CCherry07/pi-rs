// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { DeliveryAttempt, DeliveryOverview, DeliveryPreview } from "../../../services/tauri";
import type { useWorktreeDelivery } from "../hooks/useWorktreeDelivery";
import { WorktreeDeliveryDialog } from "./WorktreeDeliveryDialog";

afterEach(cleanup);
const overview: DeliveryOverview = {
  workspaceId: "managed", name: "Feature group", attempts: [], sharedRoots: [{ id: "docs", name: "Docs", path: "/shared/docs", ownership: { kind: "external" } }],
  checkouts: ["app", "api"].map((key) => ({ key, workdir: `/worktrees/${key}`, originWorkdir: `/original/${key}`, rootIds: [key],
    createdBranch: `feature-${key}`, startOid: "starting", head: { oid: "current", branch: `feature-${key}` }, changes: [], changesTruncated: false,
    targetBranches: [{ name: "main", oid: "target" }, { name: "release", oid: "other" }], defaultTargetBranch: "main", warnings: [], error: null,
  })),
};
const preview: DeliveryPreview = {
  workspaceId: "managed", checkoutKey: "app", sourceWorkdir: "/worktrees/app", targetWorkdir: "/original/app",
  source: { oid: "source-oid", branch: "feature-app" }, target: { oid: "target-oid", branch: "main" },
  sourceChanges: [{ path: "draft.txt", indexStatus: "?", worktreeStatus: "?" }], targetChanges: [{ path: "ignored.txt", indexStatus: "!", worktreeStatus: "!" }],
  sourceChangesTruncated: false, targetChangesTruncated: true, blockers: ["Source has local changes"], warnings: ["Snapshot warning"],
  comparison: { sourceOid: "source-oid", targetOid: "target-oid", mergeBaseOids: ["base"], ahead: 1, behind: 1, kind: "conflicts",
    commits: [{ oid: "incoming", summary: "Add the feature" }], commitsTruncated: true,
    files: [{ path: "new.txt", oldPath: "old.txt", status: "renamed" }], filesTruncated: false, conflicts: ["conflicted.txt"], warnings: ["Comparison warning"],
  },
};
function controller(value: DeliveryPreview | null = null): ReturnType<typeof useWorktreeDelivery> {
  return {
    state: { workspaceId: "managed", threadId: "saved", name: "Feature group", overview, checkoutKey: "app", targets: { app: "main", api: "main" }, preview: value, isLoading: false, isPreviewing: false, error: null, operation: null, operationError: null },
    canOpen: true, open: vi.fn(), close: vi.fn(), refresh: vi.fn().mockResolvedValue(undefined), selectCheckout: vi.fn(), selectTarget: vi.fn(), preview: vi.fn().mockResolvedValue(undefined),
    execute: vi.fn().mockResolvedValue(undefined), inspect: vi.fn().mockResolvedValue(undefined), finish: vi.fn().mockResolvedValue(undefined),
  };
}

it("shows owned repositories and compares a selected local target without a merge action", () => {
  const delivery = controller();
  render(<WorktreeDeliveryDialog delivery={delivery} />);
  expect(screen.getByText("/original/app")).toBeTruthy();
  expect(screen.getByText("Docs · /shared/docs")).toBeTruthy();
  const repositories = screen.getByLabelText("Managed repositories");
  expect(within(repositories).getAllByRole("button")).toHaveLength(2);
  fireEvent.click(within(repositories).getByRole("button", { name: /feature-api/ }));
  expect(delivery.selectCheckout).toHaveBeenCalledWith("api");
  fireEvent.change(screen.getByLabelText("Local target branch"), { target: { value: "release" } });
  expect(delivery.selectTarget).toHaveBeenCalledWith("release");
  fireEvent.click(screen.getByRole("button", { name: "Preview merge" }));
  expect(delivery.preview).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("button", { name: /^Merge$/i })).toBeNull();
});

it("shows pinned commits, conflicts, local files, truncation, and native blockers", () => {
  render(<WorktreeDeliveryDialog delivery={controller(preview)} />);
  const section = screen.getByRole("region", { name: "Merge preview" });
  expect(document.activeElement).toBe(section);
  for (const text of ["Conflicts found", "source-oid", "target-oid", "Add the feature", "Renamed", "old.txt → new.txt", "conflicted.txt", "Source has local changes", "Snapshot warning", "Comparison warning", "ignored.txt"]) {
    expect(within(section).getByText(text)).toBeTruthy();
  }
  expect(screen.getByText("draft.txt")).toBeTruthy();
  expect(within(section).getAllByText("Showing a partial list.")).toHaveLength(2);
});

it("localizes incoming file statuses and preserves unknown statuses", () => {
  const result: DeliveryPreview = { ...preview, comparison: { ...preview.comparison, files: [
    { path: "file.txt", oldPath: null, status: "typeChanged" },
    { path: "other.txt", oldPath: null, status: "futureStatus" },
  ] } };
  render(<WorktreeDeliveryDialog delivery={controller(result)} />);
  expect(screen.getByText("Type changed")).toBeTruthy();
  expect(screen.getByText("futureStatus")).toBeTruthy();
});

it("does not describe an unreadable checkout as clean", () => {
  const delivery = controller();
  if (!delivery.state) throw new Error("Missing test state");
  delivery.state = { ...delivery.state, overview: { ...overview, checkouts: [{ ...overview.checkouts[0], head: null, error: "Repository unavailable" }] } };
  render(<WorktreeDeliveryDialog delivery={delivery} />);
  expect(screen.getByText("Working directory status is unavailable.")).toBeTruthy();
  expect(screen.queryByText("No local changes")).toBeNull();
  expect(screen.getByRole("button", { name: "Preview merge" })).toHaveProperty("disabled", true);
});

it.each(["unrelated", "unsupported"] as const)("does not interpret an unavailable %s file comparison as an empty diff", (kind) => {
  const result: DeliveryPreview = { ...preview, comparison: { ...preview.comparison, kind, mergeBaseOids: [], files: [], conflicts: [] } };
  render(<WorktreeDeliveryDialog delivery={controller(result)} />);
  expect(screen.getByText("File comparison is unavailable without a unique merge base.")).toBeTruthy();
  expect(screen.queryByText("No incoming file changes.")).toBeNull();
});

it("offers a merge for the exact previewed target and commits, and respects native blockers", () => {
  const value: DeliveryPreview = { ...preview, blockers: [], comparison: { ...preview.comparison, kind: "fastForward", conflicts: [] } };
  const delivery = controller(value);
  const { rerender } = render(<WorktreeDeliveryDialog delivery={delivery} />);
  const section = screen.getByRole("region", { name: "Merge preview" });
  expect(within(section).getByText("/original/app")).toBeTruthy();
  expect(within(section).getByText("source-oid")).toBeTruthy();
  expect(within(section).getByText("target-oid")).toBeTruthy();
  fireEvent.click(within(section).getByRole("button", { name: "Merge into main" }));
  expect(delivery.execute).toHaveBeenCalledTimes(1);
  rerender(<WorktreeDeliveryDialog delivery={controller({ ...value, blockers: ["Target checkout is dirty"] })} />);
  expect(screen.getByRole("button", { name: "Merge into main" })).toHaveProperty("disabled", true);
  expect(screen.getByText("Target checkout is dirty")).toBeTruthy();
});

it("shows all checkout results and exposes only the recorded recovery actions", () => {
  const delivery = controller();
  if (!delivery.state) throw new Error("Missing test state");
  const base: DeliveryAttempt = { id: "ready", checkoutKey: "api", targetBranch: "release", sourceOid: "api-source", targetOid: "api-target", resultOid: "api-result",
    status: "readyToFinish", createdAt: "2026-09-22T00:00:00Z", updatedAt: "2026-09-22T00:00:00Z", error: null };
  delivery.state.overview = { ...overview, attempts: [base,
    { ...base, id: "attention", checkoutKey: "app", status: "needsAttention", error: "Original files changed" },
    { ...base, id: "done", status: "completed" },
  ] };
  render(<WorktreeDeliveryDialog delivery={delivery} />);
  const ready = screen.getByRole("region", { name: "Delivery attempt ready" });
  for (const text of ["/worktrees/api", "/original/api", "api-source", "api-target", "api-result", "Ready to finish branch update"]) {
    expect(within(ready).getByText(text)).toBeTruthy();
  }
  fireEvent.click(within(ready).getByRole("button", { name: "Finish updating release" }));
  expect(delivery.finish).toHaveBeenCalledWith("ready");
  const attention = screen.getByRole("region", { name: "Delivery attempt attention" });
  expect(within(attention).getByText(/Local files have been retained/)).toBeTruthy();
  expect(within(attention).getByText("Original files changed")).toBeTruthy();
  fireEvent.click(within(attention).getByRole("button", { name: "Check again" }));
  expect(delivery.inspect).toHaveBeenCalledWith("attention");
  expect(within(attention).queryByRole("button", { name: /Finish/ })).toBeNull();
  expect(within(screen.getByRole("region", { name: "Delivery attempt done" })).queryByRole("button")).toBeNull();
  expect(delivery.selectCheckout).not.toHaveBeenCalled();
});

it("locks action controls while a native operation runs and allows closing without cancellation", () => {
  const delivery = controller();
  if (!delivery.state) throw new Error("Missing test state");
  delivery.state.operation = { kind: "execute", checkoutKey: "app", attemptId: "pending" };
  render(<WorktreeDeliveryDialog delivery={delivery} />);
  expect(screen.getByRole("status").textContent).toContain("Closing this dialog does not cancel");
  expect(screen.getByRole("button", { name: "Refresh overview" })).toHaveProperty("disabled", true);
  expect(screen.getByRole("button", { name: "Preview merge" })).toHaveProperty("disabled", true);
  expect(screen.getByLabelText("Local target branch")).toHaveProperty("disabled", true);
  expect(within(screen.getByLabelText("Managed repositories")).getAllByRole("button").every((button) => button.hasAttribute("disabled"))).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(delivery.close).toHaveBeenCalledTimes(1);
});

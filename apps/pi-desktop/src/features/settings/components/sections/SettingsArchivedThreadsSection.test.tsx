// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ArchivedThreadEntry,
  SettingsArchivedThreadsSectionProps,
} from "@settings/hooks/useSettingsArchivedThreadsSection";
import { SettingsArchivedThreadsSection } from "./SettingsArchivedThreadsSection";

const archivedThread = (
  id: string,
  name: string,
): ArchivedThreadEntry => ({
  id,
  name,
  updatedAt: 1_700_000_000_000,
  modelId: null,
  effort: null,
  workspaceId: "workspace-1",
  workspaceName: "Project One",
  workspacePath: "/tmp/project-one",
});

const baseProps = (): SettingsArchivedThreadsSectionProps => ({
  archivedThreads: [
    archivedThread("thread-one", "First task"),
    archivedThread("thread-two", "Second task"),
  ],
  loading: false,
  error: null,
  busyThreadActions: {},
  deletingAll: false,
  onRefresh: vi.fn(),
  onRestoreThread: vi.fn(async () => {}),
  onDeleteThread: vi.fn(async () => {}),
  onDeleteAllThreads: vi.fn(async () => {}),
});

describe("SettingsArchivedThreadsSection", () => {
  afterEach(() => {
    cleanup();
  });

  it("shows only the active action as busy and leaves other rows enabled", () => {
    render(
      <SettingsArchivedThreadsSection
        {...baseProps()}
        busyThreadActions={{ "workspace-1:thread-one": "delete" }}
      />,
    );

    const firstRow = screen.getByText("First task").closest(
      ".settings-archived-row",
    ) as HTMLElement;
    const secondRow = screen.getByText("Second task").closest(
      ".settings-archived-row",
    ) as HTMLElement;

    const firstRestore = within(firstRow).getByRole("button", {
      name: "Restore",
    }) as HTMLButtonElement;
    const firstDelete = within(firstRow).getByRole("button", {
      name: "Deleting...",
    }) as HTMLButtonElement;
    expect(firstRestore.disabled).toBe(true);
    expect(firstDelete.disabled).toBe(true);
    expect(within(firstRow).queryByText("Restoring...")).toBeNull();

    const secondRestore = within(secondRow).getByRole("button", {
      name: "Restore",
    }) as HTMLButtonElement;
    const secondDelete = within(secondRow).getByRole("button", {
      name: "Delete",
    }) as HTMLButtonElement;
    expect(secondRestore.disabled).toBe(false);
    expect(secondDelete.disabled).toBe(false);
  });
});

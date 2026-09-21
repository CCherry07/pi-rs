/** @vitest-environment jsdom */
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceFileListing } from "../../../types";
import { readWorkspaceFile } from "../../../services/tauri";
import { FileTreePanel } from "./FileTreePanel";

vi.mock("../../../services/tauri", () => ({ readWorkspaceFile: vi.fn() }));
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getVirtualItems: () => Array.from({ length: count }, (_, index) => ({ index, key: index, start: index * 28 })),
    getTotalSize: () => count * 28,
    measureElement: () => {},
  }),
}));
vi.mock("./FilePreviewPopover", () => ({
  FilePreviewPopover: ({ absolutePath, content }: { absolutePath: string; content: string }) => <div data-testid="preview">{absolutePath} {content}</div>,
}));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const listing: WorkspaceFileListing = {
  workspace: { roots: [
    { id: "app", name: "app", path: "/old/app", ownership: { kind: "external" } },
    { id: "shared", name: "shared", path: "/old/shared", ownership: { kind: "external" } },
  ], primaryRoot: "app", executionDir: "/old/app/src" },
  files: [{ rootId: "app", path: "src/index.ts" }, { rootId: "shared", path: "src/index.ts" }], errors: [],
};

describe("multi-root file panel", () => {
  it("previews and mentions the selected root and scopes modified files to their repository", async () => {
    vi.mocked(readWorkspaceFile).mockResolvedValue({ content: "shared contents", truncated: false });
    const onInsertText = vi.fn();
    render(<FileTreePanel workspaceId="project" threadId="saved" listing={listing}
      error={null} onRefresh={vi.fn()} modifiedRoot="/old/app/" modifiedFiles={["src/index.ts"]}
      isLoading={false} filePanelMode="files" onFilePanelModeChange={vi.fn()}
      onInsertText={onInsertText} canInsertText openTargets={[]} openAppIconById={{}}
      selectedOpenAppId="" onSelectOpenAppId={vi.fn()} />);
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "shared" } });
    fireEvent.click(screen.getByRole("button", { name: "index.ts" }));
    await waitFor(() => expect(readWorkspaceFile).toHaveBeenCalledWith("project", { rootId: "shared", path: "src/index.ts" }, "saved"));
    await waitFor(() => expect(screen.getByTestId("preview").textContent).toContain("/old/shared/src/index.ts shared contents"));
    fireEvent.click(screen.getByRole("button", { name: "Mention index.ts" }));
    expect(onInsertText).toHaveBeenCalledWith("/old/shared/src/index.ts");
    fireEvent.click(screen.getByRole("button", { name: "Show modified files only" }));
    expect(screen.queryByRole("button", { name: "index.ts" })).toBeNull();
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "app" } });
    expect(screen.queryByTestId("preview")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show modified files only" }));
    expect(screen.getByRole("button", { name: "index.ts" })).toBeTruthy();
  });
});

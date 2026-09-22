// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CreateWorkspacePrompt, type CreateWorkspacePromptProps } from "./CreateWorkspacePrompt";

afterEach(cleanup);

const appRoot = { id: "app-root", name: "app", path: "/projects/app" };
const docsRoot = { id: "docs-root", name: "docs", path: "/projects/docs" };
const exampleRoot = { id: "example-root", name: "app", path: "/examples/app" };

function makeProps(overrides: Partial<CreateWorkspacePromptProps> = {}): CreateWorkspacePromptProps {
  return {
    name: "app",
    roots: [appRoot],
    primaryRootId: appRoot.id,
    error: null,
    isBusy: false,
    isChoosing: false,
    onNameChange: vi.fn(),
    onChooseDirectories: vi.fn(),
    onRemoveDirectory: vi.fn(),
    onPrimaryRootChange: vi.fn(),
    onCancel: vi.fn(),
    onConfirm: vi.fn(),
    ...overrides,
  };
}

describe("CreateWorkspacePrompt", () => {
  it("starts with a directory picker and cannot create an empty workspace", () => {
    const props = makeProps({ name: "", roots: [], primaryRootId: null });
    render(<CreateWorkspacePrompt {...props} />);

    expect(screen.getByRole("dialog", { name: "Add workspace" })).toBeTruthy();
    expect(document.activeElement).toBe(screen.getByLabelText("Workspace name"));
    expect(screen.getByText("Add a repository or any other folder to get started.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add directories" }));
    expect(props.onChooseDirectories).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    expect(props.onConfirm).not.toHaveBeenCalled();
  });

  it("keeps one directory simple and submits the form with the provided name", () => {
    const props = makeProps();
    const { container } = render(<CreateWorkspacePrompt {...props} />);

    expect(screen.getByText("/projects/app")).toBeTruthy();
    expect(screen.queryByRole("radio")).toBeNull();
    fireEvent.change(screen.getByLabelText("Workspace name"), {
      target: { value: "My app" },
    });
    expect(props.onNameChange).toHaveBeenCalledWith("My app");
    const form = container.querySelector("form");
    if (!form) throw new Error("Expected workspace creation form");
    fireEvent.submit(form);
    expect(props.onConfirm).toHaveBeenCalledOnce();
  });

  it("distinguishes matching directory names by path when selecting or removing a root", () => {
    const props = makeProps({
      roots: [appRoot, exampleRoot],
    });
    const { rerender } = render(<CreateWorkspacePrompt {...props} />);

    expect(screen.getByText("New sessions start in the primary directory.")).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", {
      name: "Use /examples/app as the primary directory",
    }));
    expect(props.onPrimaryRootChange).toHaveBeenCalledWith(exampleRoot.id);
    rerender(<CreateWorkspacePrompt {...props} primaryRootId={exampleRoot.id} />);
    const primary = screen.getByRole<HTMLInputElement>("radio", {
      name: "Use /examples/app as the primary directory",
    });
    expect(primary.checked).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Remove /projects/app" }));
    expect(props.onRemoveDirectory).toHaveBeenCalledWith(appRoot.id);
  });

  it("preserves distinct roots at the same path when selecting or removing a directory", () => {
    const aliasRoot = { ...appRoot, id: "app-alias", name: "App resources" };
    const props = makeProps({ roots: [appRoot, aliasRoot] });
    const { rerender } = render(<CreateWorkspacePrompt {...props} />);
    const aliasRow = screen.getByText("App resources").closest("li");
    if (!aliasRow) throw new Error("Expected the aliased directory row");
    const radio = within(aliasRow).getByRole<HTMLInputElement>("radio");
    expect(radio.checked).toBe(false);
    fireEvent.click(radio);
    expect(props.onPrimaryRootChange).toHaveBeenCalledExactlyOnceWith(aliasRoot.id);

    rerender(<CreateWorkspacePrompt {...props} primaryRootId={aliasRoot.id} />);
    expect(radio.checked).toBe(true);
    const roots = screen.getAllByRole<HTMLInputElement>("radio");
    expect(roots[0].checked).toBe(false);
    fireEvent.click(within(aliasRow).getByRole("button", { name: "Remove /projects/app" }));
    expect(props.onRemoveDirectory).toHaveBeenCalledExactlyOnceWith(aliasRoot.id);
  });

  it.each([
    { name: "   " },
    { primaryRootId: null },
    { primaryRootId: "missing-root" },
    { roots: [] },
  ])("guards invalid form submission: %j", (overrides) => {
    const props = makeProps(overrides);
    const { container } = render(<CreateWorkspacePrompt {...props} />);
    const form = container.querySelector("form");
    if (!form) throw new Error("Expected workspace creation form");

    fireEvent.submit(form);
    expect(props.onConfirm).not.toHaveBeenCalled();
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Create" }).disabled).toBe(true);
  });

  it.each([
    { isBusy: true, isChoosing: false },
    { isBusy: false, isChoosing: true },
  ])("holds the form steady during an operation: %j", (state) => {
    const props = makeProps({ ...state, roots: [appRoot, docsRoot] });
    const { container } = render(<CreateWorkspacePrompt {...props} />);
    const name = screen.getByLabelText<HTMLInputElement>("Workspace name");
    expect(name.disabled).toBe(true);
    for (const button of screen.getAllByRole<HTMLButtonElement>("button")) {
      expect(button.disabled).toBe(true);
      fireEvent.click(button);
    }
    for (const radio of screen.getAllByRole<HTMLInputElement>("radio")) {
      expect(radio.disabled).toBe(true);
    }
    fireEvent.keyDown(name, { key: "Escape" });
    const backdrop = container.querySelector(".ds-modal-backdrop");
    if (!backdrop) throw new Error("Expected modal backdrop");
    fireEvent.click(backdrop);
    expect(props.onCancel).not.toHaveBeenCalled();
    expect(props.onConfirm).not.toHaveBeenCalled();
    expect(props.onChooseDirectories).not.toHaveBeenCalled();
    expect(props.onRemoveDirectory).not.toHaveBeenCalled();
  });

  it("closes with Escape from any control or the backdrop and restores focus", () => {
    const opener = document.createElement("button");
    document.body.append(opener);
    opener.focus();
    const props = makeProps();
    const { container, unmount } = render(<CreateWorkspacePrompt {...props} />);

    fireEvent.keyDown(screen.getByRole("button", { name: "Add directories" }), { key: "Escape" });
    expect(props.onCancel).toHaveBeenCalledOnce();
    const backdrop = container.querySelector(".ds-modal-backdrop");
    if (!backdrop) throw new Error("Expected modal backdrop");
    fireEvent.click(backdrop);
    expect(props.onCancel).toHaveBeenCalledTimes(2);
    unmount();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it("keeps keyboard focus within the panel and exposes creation errors", () => {
    render(<CreateWorkspacePrompt {...makeProps({ error: "Directory no longer exists." })} />);
    const first = screen.getByLabelText("Workspace name");
    const last = screen.getByRole("button", { name: "Create" });
    fireEvent.keyDown(first, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
    fireEvent.keyDown(last, { key: "Tab" });
    expect(document.activeElement).toBe(first);
    expect(screen.getByRole("alert").textContent).toBe("Directory no longer exists.");
  });

  it("reuses the directory form to save an existing workspace", () => {
    const props = makeProps({ mode: "edit" });
    const { rerender } = render(<CreateWorkspacePrompt {...props} />);

    expect(screen.getByRole("dialog", { name: "Edit workspace" })).toBeTruthy();
    expect(screen.getByText("Directory changes apply to new sessions.")).toBeTruthy();
    expect(screen.getByLabelText<HTMLInputElement>("Workspace name").value).toBe("app");
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(props.onConfirm).toHaveBeenCalledOnce();

    rerender(<CreateWorkspacePrompt {...props} isBusy />);
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Saving…" }).disabled).toBe(true);
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Cancel" }).disabled).toBe(true);
  });

  it("blocks editing while loading but allows dismissal from inside the dialog", () => {
    const props = makeProps({ mode: "edit", isLoading: true });
    const { container } = render(<CreateWorkspacePrompt {...props} />);

    expect(screen.getByRole("status").textContent).toBe("Loading workspace…");
    expect(screen.getByLabelText<HTMLInputElement>("Workspace name").disabled).toBe(true);
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Add directories" }).disabled).toBe(true);
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true);
    const cancel = screen.getByRole<HTMLButtonElement>("button", { name: "Cancel" });
    expect(cancel.disabled).toBe(false);
    expect(document.activeElement).toBe(cancel);
    fireEvent.keyDown(cancel, { key: "Escape" });
    const backdrop = container.querySelector(".ds-modal-backdrop");
    if (!backdrop) throw new Error("Expected modal backdrop");
    fireEvent.click(backdrop);
    expect(props.onCancel).toHaveBeenCalledTimes(2);
    expect(props.onConfirm).not.toHaveBeenCalled();
  });

  it("offers a load retry without letting an incomplete edit overwrite the workspace", () => {
    const onRetryLoad = vi.fn();
    const props = makeProps({
      mode: "edit",
      error: "Project could not be read.",
      onRetryLoad,
    });
    const { rerender } = render(<CreateWorkspacePrompt {...props} />);

    expect(screen.getByRole("alert").textContent).toContain("Project could not be read.");
    expect(screen.getByLabelText<HTMLInputElement>("Workspace name").disabled).toBe(true);
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetryLoad).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(props.onCancel).toHaveBeenCalledOnce();

    rerender(<CreateWorkspacePrompt {...props} error={null} onRetryLoad={undefined} />);
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    expect(screen.getByLabelText<HTMLInputElement>("Workspace name").disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(props.onConfirm).toHaveBeenCalledOnce();
  });

  it("allows correcting and retrying a failed save", () => {
    const props = makeProps({ mode: "edit", error: "Directory no longer exists." });
    render(<CreateWorkspacePrompt {...props} />);

    expect(screen.getByRole("alert").textContent).toBe("Directory no longer exists.");
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Add directories" }));
    expect(props.onChooseDirectories).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(props.onConfirm).toHaveBeenCalledOnce();
  });
});

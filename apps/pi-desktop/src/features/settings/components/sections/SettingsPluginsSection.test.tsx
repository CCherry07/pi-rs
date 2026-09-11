// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ask, open } from "@tauri-apps/plugin-dialog";
import type { WorkspaceInfo } from "@/types";
import { readPlugins, operatePlugins, readPluginRuntime, type PluginLibrarySnapshot, type PluginRuntimeSnapshot } from "@/services/plugins";
import { SettingsPluginsSection, type PluginSessionContext } from "./SettingsPluginsSection";

vi.mock("@/services/plugins", () => ({ readPlugins: vi.fn(), operatePlugins: vi.fn(), readPluginRuntime: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(), open: vi.fn() }));
const project = { id: "project", name: "Project" } as WorkspaceInfo;
const other = { id: "other", name: "Other" } as WorkspaceInfo;
const doc: PluginLibrarySnapshot = {
  scope: "global", path: "/agent/plugins.json", lockPath: "/agent/plugins.lock", target: "host", writable: true,
  projectTrusted: false, intentCurrent: true, diagnostics: [],
  plugins: [{ id: "example", source: "registry:example", configured: true, requestedVersion: "^1", installed: { id: "example", source: "https://example.com/release.json", version: "1.2.0", kind: "agent", target: "host", sha256: "abc123" } }],
};
const inventory: PluginRuntimeSnapshot = { workspaceId: "project", threadId: "thread", configuredNativePluginIds: ["example"] };
const session = (): PluginSessionContext => ({ workspaceId: "project", workspaceName: "Project", threadId: "thread", isProcessing: false, reload: vi.fn().mockResolvedValue(undefined) });
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(done => { resolve = done; }); return { promise, resolve }; }
function mount(context?: PluginSessionContext) {
  const dirty = vi.fn();
  return { dirty, ...render(<SettingsPluginsSection projects={[project, other]} onDirtyChange={dirty} session={context} />) };
}
const button = (name: string) => screen.getByRole("button", { name });
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(readPlugins).mockResolvedValue(doc);
  vi.mocked(operatePlugins).mockResolvedValue(doc);
  vi.mocked(readPluginRuntime).mockResolvedValue(inventory);
  vi.mocked(ask).mockResolvedValue(true);
});
afterEach(cleanup);

describe("SettingsPluginsSection", () => {
  it("lists read-only package records without constructing a session, supports filtering and metadata", async () => {
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "example" }));
    expect(readPlugins).toHaveBeenCalledExactlyOnceWith(null);
    expect(readPluginRuntime).not.toHaveBeenCalled();
    expect(operatePlugins).not.toHaveBeenCalled();
    expect(screen.getByText(/Runtime not observed/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reload selected session" })).toBeNull();
    expect(screen.getByText("abc123")).toBeTruthy();
    expect(screen.getByText("1.2.0")).toBeTruthy();
    fireEvent.click(button("Back to plugins"));
    fireEvent.change(screen.getByLabelText("Plugin type"), { target: { value: "provider" } });
    expect(screen.queryByRole("button", { name: "example" })).toBeNull();
    fireEvent.change(screen.getByLabelText("Plugin type"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Search plugin ID or source"), { target: { value: "registry" } });
    expect(button("example")).toBeTruthy();
  });

  it("requires explicit source trust, selects absolute folder and passes install options once", async () => {
    const pending = deferred<PluginLibrarySnapshot>();
    vi.mocked(operatePlugins).mockReturnValue(pending.promise);
    vi.mocked(open).mockResolvedValue("/absolute/local-package");
    const { dirty } = mount();
    await screen.findByRole("button", { name: "example" });
    fireEvent.click(button("Install plugin"));
    fireEvent.click(button("Choose local package folder"));
    await waitFor(() => expect(screen.getByLabelText("Source")).toHaveProperty("value", "/absolute/local-package"));
    expect(dirty).toHaveBeenLastCalledWith(true);
    expect(button("Install trusted plugin")).toHaveProperty("disabled", true);
    fireEvent.change(screen.getByLabelText("Version constraint (optional)"), { target: { value: "^1" } });
    fireEvent.change(screen.getByLabelText("Registry URL (optional)"), { target: { value: "https://registry.example/index.json" } });
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(button("Install trusted plugin"));
    fireEvent.click(button("Install trusted plugin"));
    expect(operatePlugins).toHaveBeenCalledExactlyOnceWith(null, { kind: "install", source: "/absolute/local-package", version: "^1", registry: "https://registry.example/index.json" });
    await act(async () => pending.resolve(doc));
    expect(dirty).toHaveBeenLastCalledWith(false);
    expect(screen.getByText(/Package operation completed/)).toBeTruthy();
    expect(screen.getByText(/Package state may have changed/)).toBeTruthy();
    expect(readPlugins).toHaveBeenCalledOnce();
  });

  it("keeps dirty install drafts when scope navigation is cancelled", async () => {
    vi.mocked(ask).mockResolvedValue(false);
    mount();
    await screen.findByRole("button", { name: "example" });
    fireEvent.click(button("Install plugin"));
    fireEvent.change(screen.getByLabelText("Source"), { target: { value: "registry:example" } });
    fireEvent.change(screen.getByLabelText("Configuration scope"), { target: { value: "project" } });
    await waitFor(() => expect(ask).toHaveBeenCalledOnce());
    expect(screen.getByLabelText("Configuration scope")).toHaveProperty("value", "");
    expect(screen.getByLabelText("Source")).toHaveProperty("value", "registry:example");
    expect(readPlugins).toHaveBeenCalledOnce();
  });

  it("syncs without updating or reloading and confirms removal", async () => {
    const context = session(); mount(context);
    fireEvent.click(await screen.findByRole("button", { name: "example" }));
    fireEvent.click(button("Sync"));
    await waitFor(() => expect(operatePlugins).toHaveBeenCalledWith(null, { kind: "sync" }));
    await waitFor(() => expect(button("Remove plugin")).toHaveProperty("disabled", false));
    expect(context.reload).not.toHaveBeenCalled();
    vi.mocked(ask).mockResolvedValueOnce(false);
    fireEvent.click(button("Remove plugin"));
    await waitFor(() => expect(ask).toHaveBeenCalledOnce());
    expect(operatePlugins).toHaveBeenCalledOnce();
    await waitFor(() => expect(button("Remove plugin")).toHaveProperty("disabled", false));
    fireEvent.click(button("Remove plugin"));
    await waitFor(() => expect(operatePlugins).toHaveBeenLastCalledWith(null, { kind: "remove", id: "example" }));
    expect(vi.mocked(ask).mock.calls[1][0]).toContain("Global");
  });

  it("disables untrusted project actions and surfaces backend trust failures", async () => {
    vi.mocked(readPlugins).mockImplementation(async id => id ? { ...doc, scope: "project", writable: false, projectTrusted: false, plugins: [] } : doc);
    mount();
    await screen.findByRole("button", { name: "example" });
    fireEvent.change(screen.getByLabelText("Configuration scope"), { target: { value: "project" } });
    await screen.findByText(/This project is not trusted/);
    expect(button("Sync")).toHaveProperty("disabled", true);
    expect(button("Install plugin")).toHaveProperty("disabled", true);
    expect(operatePlugins).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("Configuration scope"), { target: { value: "" } });
    await screen.findByRole("button", { name: "example" });
    vi.mocked(operatePlugins).mockRejectedValue("Project plugin configuration requires project trust");
    fireEvent.click(button("Sync"));
    await screen.findByText(/Project plugin configuration requires project trust/);
    expect(screen.getByText(/package state may already have changed/)).toBeTruthy();
    expect(button("Sync")).toHaveProperty("disabled", true);
    fireEvent.click(button("Refresh plugin snapshots"));
    await screen.findByRole("button", { name: "example" });
  });

  it("separates ID inventory from installation metadata and reloads only through the app controller", async () => {
    const context = session(); mount(context);
    await screen.findByText("Configured Native plugin IDs observed in this generation:");
    expect(readPluginRuntime).toHaveBeenCalledExactlyOnceWith("project", "thread");
    const runtime = screen.getByText("Selected session inventory").closest(".settings-plugins-runtime")! as HTMLElement;
    expect(within(runtime).getByText("example")).toBeTruthy();
    expect(within(runtime).queryByText(/1.2.0/)).toBeNull();
    expect(within(runtime).getByText(/cannot confirm the loaded version/)).toBeTruthy();
    await waitFor(() => expect(button("Reload selected session")).toHaveProperty("disabled", false));
    fireEvent.click(button("Reload selected session"));
    await waitFor(() => expect(context.reload).toHaveBeenCalledOnce());
    await waitFor(() => expect(readPlugins).toHaveBeenCalledTimes(2));
    expect(readPluginRuntime).toHaveBeenCalledTimes(2);
    expect(operatePlugins).not.toHaveBeenCalled();
  });

  it("preserves changed state after reload failure and reports successful reload separately from failed refresh", async () => {
    const context = session(); vi.mocked(context.reload).mockRejectedValueOnce("incompatible ABI");
    mount(context);
    await screen.findByRole("button", { name: "example" });
    fireEvent.click(button("Sync"));
    await screen.findByText(/Package operation completed/);
    fireEvent.click(button("Reload selected session"));
    await screen.findByText(/incompatible ABI/);
    expect(screen.getByText(/Package state may have changed/)).toBeTruthy();
    vi.mocked(readPlugins).mockRejectedValueOnce("snapshot unavailable");
    fireEvent.click(button("Reload selected session"));
    await screen.findByText(/snapshot unavailable/);
    expect(screen.getByText(/Selected session reloaded/)).toBeTruthy();
    expect(screen.queryByText(/Package state may have changed/)).toBeNull();
  });

  it("ignores late scope and runtime reads when viewing a different project", async () => {
    const old = deferred<PluginLibrarySnapshot>();
    const oldRuntime = deferred<PluginRuntimeSnapshot>();
    vi.mocked(readPlugins).mockReturnValueOnce(old.promise).mockResolvedValue({ ...doc, scope: "project", plugins: [] });
    vi.mocked(readPluginRuntime).mockReturnValue(oldRuntime.promise);
    mount(session());
    fireEvent.change(screen.getByLabelText("Configuration scope"), { target: { value: "other" } });
    await waitFor(() => expect(readPlugins).toHaveBeenCalledWith("other"));
    await act(async () => { old.resolve(doc); oldRuntime.resolve(inventory); });
    expect(screen.queryByRole("button", { name: "example" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Reload selected session" })).toBeNull();
    expect(screen.queryByText("Configured Native plugin IDs observed in this generation:")).toBeNull();
    expect(readPluginRuntime).toHaveBeenCalledOnce();
  });

  it("does not observe a missing thread, but routes explicit draft reload via its controller", async () => {
    const context = { ...session(), threadId: null }; mount(context);
    await screen.findByRole("button", { name: "example" });
    expect(readPluginRuntime).not.toHaveBeenCalled();
    expect(context.reload).not.toHaveBeenCalled();
    fireEvent.click(button("Reload current workspace draft"));
    await waitFor(() => expect(context.reload).toHaveBeenCalledOnce());
    expect(readPluginRuntime).not.toHaveBeenCalled();
  });

  it("blocks duplicate reloads and ignores responses after unmount", async () => {
    const pending = deferred<void>();
    const context = session(); vi.mocked(context.reload).mockReturnValue(pending.promise);
    const view = mount(context);
    await screen.findByRole("button", { name: "example" });
    fireEvent.click(button("Reload selected session"));
    fireEvent.click(button("Reload selected session"));
    await waitFor(() => expect(context.reload).toHaveBeenCalledOnce());
    view.unmount();
    await act(async () => pending.resolve());
    expect(readPlugins).toHaveBeenCalledOnce();
  });
});

it("does not reload a previous selection after confirmation resolves for another thread", async () => {
  const pending = deferred<boolean>();
  vi.mocked(ask).mockReturnValue(pending.promise);
  const first = session();
  const next = { ...session(), threadId: "next" };
  const { rerender, dirty } = mount(first);
  await screen.findByRole("button", { name: "example" });
  fireEvent.click(button("Reload selected session"));
  rerender(<SettingsPluginsSection projects={[project, other]} onDirtyChange={dirty} session={next} />);
  await act(async () => pending.resolve(true));
  expect(first.reload).not.toHaveBeenCalled();
  expect(next.reload).not.toHaveBeenCalled();
});

it("copies metadata and diagnostics, and never substitutes installed records for a closed runtime", async () => {
  const clipboard = { writeText: vi.fn().mockResolvedValue(undefined) };
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: clipboard });
  vi.mocked(readPluginRuntime).mockRejectedValue("selected thread was closed");
  mount(session());
  fireEvent.click(await screen.findByRole("button", { name: "example" }));
  await screen.findByText(/selected thread was closed/);
  expect(screen.getByText(/Runtime not observed/)).toBeTruthy();
  fireEvent.click(button("Copy source"));
  await waitFor(() => expect(clipboard.writeText).toHaveBeenCalledWith("registry:example"));
  fireEvent.click(button("Copy SHA-256"));
  await waitFor(() => expect(clipboard.writeText).toHaveBeenCalledWith("abc123"));
  fireEvent.click(button("Copy diagnostics"));
  await waitFor(() => expect(clipboard.writeText).toHaveBeenCalledWith("selected thread was closed"));
});

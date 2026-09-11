// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ask } from "@tauri-apps/plugin-dialog";
import { readPlugins, readPluginRuntime } from "@/services/plugins";
import { SettingsView, type SettingsViewProps } from "./SettingsView";

vi.mock("@settings/hooks/useSettingsViewOrchestration", () => ({ useSettingsViewOrchestration: () => ({ projectsSectionProps: { projects: [], workspaceGroups: [], groupedWorkspaces: [] } }) }));
vi.mock("@/services/plugins", () => ({ readPlugins: vi.fn(), readPluginRuntime: vi.fn(), operatePlugins: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(), open: vi.fn() }));
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(readPlugins).mockResolvedValue({ scope: "global", path: "/agent/plugins.json", lockPath: "/agent/plugins.lock", target: "host", writable: true, projectTrusted: false, intentCurrent: true, diagnostics: [], plugins: [] });
  vi.mocked(readPluginRuntime).mockResolvedValue({ workspaceId: "workspace", threadId: "thread", configuredNativePluginIds: [] });
});
afterEach(cleanup);

it("routes Plugins through Settings navigation, forwards the selected context and protects dirty installation drafts on close", async () => {
  const onClose = vi.fn();
  const reload = vi.fn().mockResolvedValue(undefined);
  // Unrelated section orchestration is mocked; these are the Plugin page's actual shell inputs.
  const props = {
    initialSection: "plugins", onClose,
    pluginSession: { workspaceId: "workspace", workspaceName: "Workspace", threadId: "thread", isProcessing: false, reload },
  } as unknown as SettingsViewProps;
  render(<SettingsView {...props} />);
  await waitFor(() => expect(readPluginRuntime).toHaveBeenCalledWith("workspace", "thread"));
  const nav = screen.getByRole("button", { name: "Plugins" });
  expect(nav.classList.contains("settings-nav")).toBe(true);
  await waitFor(() => expect(screen.getByRole("button", { name: "Reload selected session" })).toHaveProperty("disabled", false));
  vi.mocked(ask).mockResolvedValueOnce(true);
  fireEvent.click(screen.getByRole("button", { name: "Reload selected session" }));
  await waitFor(() => expect(reload).toHaveBeenCalledOnce());
  await waitFor(() => expect(screen.getByRole("button", { name: "Install plugin" })).toHaveProperty("disabled", false));
  fireEvent.click(screen.getByRole("button", { name: "Install plugin" }));
  fireEvent.change(screen.getByLabelText("Source"), { target: { value: "registry:example" } });
  vi.mocked(ask).mockResolvedValue(false);
  fireEvent.click(screen.getByRole("button", { name: "Close settings" }));
  await waitFor(() => expect(ask).toHaveBeenCalledTimes(2));
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByLabelText("Source")).toHaveProperty("value", "registry:example");
  vi.mocked(ask).mockResolvedValue(true);
  fireEvent.click(screen.getByRole("button", { name: "Close settings" }));
  await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
});

it("keeps Settings open during plugin reload, preventing duplicate work from a reopened page", async () => {
  let finish!: () => void;
  const reload = vi.fn(() => new Promise<void>(resolve => { finish = resolve; }));
  const onClose = vi.fn();
  vi.mocked(ask).mockResolvedValue(true);
  render(<SettingsView {...({ initialSection: "plugins", onClose, pluginSession: { workspaceId: "workspace", workspaceName: "Workspace", threadId: "thread", isProcessing: false, reload } } as unknown as SettingsViewProps)} />);
  await waitFor(() => expect(screen.getByRole("button", { name: "Reload selected session" })).toHaveProperty("disabled", false));
  fireEvent.click(screen.getByRole("button", { name: "Reload selected session" }));
  await waitFor(() => expect(reload).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "Close settings" }));
  expect(onClose).not.toHaveBeenCalled();
  finish();
  await waitFor(() => expect(screen.getByRole("button", { name: "Reload selected session" })).toHaveProperty("disabled", false));
  fireEvent.click(screen.getByRole("button", { name: "Close settings" }));
  await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
});

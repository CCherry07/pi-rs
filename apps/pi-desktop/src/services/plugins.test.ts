import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { readPlugins, operatePlugins, readPluginRuntime } from "./plugins";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
beforeEach(() => vi.resetAllMocks());

it("uses the frozen plugin IPC contract without creating or reloading a session", async () => {
  vi.mocked(invoke).mockResolvedValue({ marker: "snapshot" });
  expect(await readPlugins(null)).toEqual({ marker: "snapshot" });
  await readPlugins("project");
  await operatePlugins("project", { kind: "install", source: "/absolute/package", version: "^1", registry: "https://example.com/index.json" });
  await operatePlugins(null, { kind: "sync" });
  await operatePlugins(null, { kind: "remove", id: "sample" });
  await readPluginRuntime("project", "existing");
  expect(vi.mocked(invoke).mock.calls).toEqual([
    ["pi_plugins_read", { workspaceId: null }],
    ["pi_plugins_read", { workspaceId: "project" }],
    ["pi_plugins_operation", { workspaceId: "project", operation: { kind: "install", source: "/absolute/package", version: "^1", registry: "https://example.com/index.json" } }],
    ["pi_plugins_operation", { workspaceId: null, operation: { kind: "sync" } }],
    ["pi_plugins_operation", { workspaceId: null, operation: { kind: "remove", id: "sample" } }],
    ["pi_plugins_runtime", { workspaceId: "project", threadId: "existing" }],
  ]);
});
it("preserves backend trust/operation failures", async () => {
  vi.mocked(invoke).mockRejectedValue("Project plugin configuration requires project trust");
  await expect(operatePlugins("project", { kind: "sync" })).rejects.toBe("Project plugin configuration requires project trust");
});

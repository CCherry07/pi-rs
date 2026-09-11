import { invoke } from "@tauri-apps/api/core";

export type InstalledPlugin = {
  id: string;
  version: string;
  kind: string;
  source: string;
  target: string;
  sha256: string;
};
export type PluginPackage = {
  id: string;
  source: string;
  requestedVersion: string | null;
  configured: boolean;
  installed: InstalledPlugin | null;
};
export type PluginLibrarySnapshot = {
  scope: "global" | "project";
  path: string;
  lockPath: string;
  target: string;
  writable: boolean;
  projectTrusted: boolean;
  plugins: PluginPackage[];
  intentCurrent: boolean | null;
  diagnostics: string[];
};
export type PluginOperation =
  | { kind: "install"; source: string; version?: string; registry?: string }
  | { kind: "sync"; registry?: string }
  | { kind: "remove"; id: string };

// Membership only: inventory cannot identify a loaded version, scope or hash.
export type PluginRuntimeSnapshot = {
  workspaceId: string;
  threadId: string;
  configuredNativePluginIds: string[];
};

export function readPlugins(workspaceId: string | null): Promise<PluginLibrarySnapshot> {
  return invoke("pi_plugins_read", { workspaceId });
}
export function operatePlugins(workspaceId: string | null, operation: PluginOperation): Promise<PluginLibrarySnapshot> {
  return invoke("pi_plugins_operation", { workspaceId, operation });
}
export function readPluginRuntime(workspaceId: string, threadId: string): Promise<PluginRuntimeSnapshot> {
  return invoke("pi_plugins_runtime", { workspaceId, threadId });
}

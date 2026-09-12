import { describe, expect, it, vi } from "vitest";
import { defineDesktopExtension, type DesktopExtension, type DesktopToolItem } from "@pi-rs/desktop-sdk";
import { DesktopExtensionRegistry, prepareDesktopExtensions } from "./registry";
import type { DesktopExtensionSource, LoadedDesktopExtension } from "./types";

const Component = () => null;
const item: DesktopToolItem = { id: "call", toolName: "check", toolType: "tool", title: "Check", detail: "" };
const definition = (id = "check", target = "check") => defineDesktopExtension({
  id,
  views: [{ id: "result", slot: "tool.result", target: { tool: target }, component: Component }],
});
const source = (id = "check", revision = "first"): DesktopExtensionSource => ({ id, revision, javascript: "bundle", css: ".check {}", scope: "global" });

describe("desktop extension generation", () => {
  it("matches exact tool/custom identities and takes an immutable registration snapshot", () => {
    const original = { apiVersion: 1, id: "tools", views: [
      { id: "result", slot: "tool.result", target: { tool: "check" }, component: Component },
      { id: "custom", slot: "tool.result", target: { customType: "check.report" }, component: Component },
    ] } satisfies DesktopExtension;
    const registry = new DesktopExtensionRegistry([{ definition: original, revision: "first" }]);
    original.views[0].target.tool = "other";
    original.views.length = 0;
    expect(registry.itemView(item)?.view.id).toBe("result");
    expect(registry.itemView({ ...item, toolName: "checker" })).toBeUndefined();
    expect(registry.itemView({ ...item, customType: "check.report" })?.view.id).toBe("custom");
    expect(registry.itemView({ ...item, toolName: undefined, data: { customType: "check.report" } })?.view.id).toBe("custom");
    expect(Object.isFrozen(registry.extensions[0].definition.views)).toBe(true);
  });

  it("rejects duplicate identities and unsupported contracts", () => {
    const extension = { definition: definition(), revision: "first" };
    expect(() => new DesktopExtensionRegistry([extension, extension])).toThrow("Duplicate desktop extension");
    expect(() => new DesktopExtensionRegistry([extension, { definition: definition("another"), revision: "first" }])).toThrow("Duplicate desktop renderer");
    const badDefinition = { ...definition(), apiVersion: 2 };
    expect(() => new DesktopExtensionRegistry([{ definition: badDefinition as unknown as DesktopExtension, revision: "first" }])).toThrow("API version 1");
    const badTarget = { id: "bad", slot: "tool.result", target: { tool: "check", customType: "check" }, component: Component };
    expect(() => new DesktopExtensionRegistry([{ definition: { apiVersion: 1, id: "bad", views: [badTarget] } as unknown as DesktopExtension, revision: "first" }])).toThrow("one exact");
  });

  it("reuses unchanged successful modules and preserves the previous generation on failed preparation", async () => {
    const importer = vi.fn().mockResolvedValue({ default: definition() });
    const first = await prepareDesktopExtensions([source()], [], new DesktopExtensionRegistry(), importer);
    const second = await prepareDesktopExtensions([source()], [], first, importer);
    expect(importer).toHaveBeenCalledTimes(1);
    expect(second.itemView(item)?.extensionId).toBe("check");
    importer.mockRejectedValueOnce(new Error("broken package"));
    await expect(prepareDesktopExtensions([source("check", "next")], [], second, importer)).rejects.toThrow("broken package");
    expect(second.itemView(item)?.revision).toBe("first");
    importer.mockResolvedValueOnce({ default: definition("wrong") });
    await expect(prepareDesktopExtensions([source("check", "next")], [], second, importer)).rejects.toThrow("different id");
  });

  it("replaces an entire bundled definition while rejecting duplicate installed packages", async () => {
    const builtin: LoadedDesktopExtension = { definition: definition(), revision: "builtin", scope: "builtin" };
    const importer = vi.fn().mockResolvedValue({ default: definition("check", "new_check") });
    const registry = await prepareDesktopExtensions([source()], [builtin], new DesktopExtensionRegistry([builtin]), importer);
    expect(registry.extensions).toHaveLength(1);
    expect(registry.itemView(item)).toBeUndefined();
    expect(registry.itemView({ ...item, toolName: "new_check" })?.revision).toBe("first");
    await expect(prepareDesktopExtensions([source(), source()], [], registry, importer)).rejects.toThrow("Duplicate desktop extension package");
    expect(importer).toHaveBeenCalledTimes(1);
  });
});

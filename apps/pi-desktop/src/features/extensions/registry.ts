import type { DesktopExtension, DesktopToolItem, DesktopView } from "@pi-rs/desktop-sdk";
import type {
  DesktopExtensionImporter,
  DesktopExtensionSource,
  LoadedDesktopExtension,
} from "./types";

export type RegisteredDesktopView<T extends DesktopView = DesktopView> = {
  extensionId: string;
  revision: string;
  css: string;
  view: T;
};

type ItemView = Extract<DesktopView, { slot: "tool.result" }>;
type PanelView = Extract<DesktopView, { slot: "workspace.panel" }>;

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

function identity(value: unknown, label: string): asserts value is string {
  if (typeof value !== "string" || !value.trim() || value !== value.trim()) {
    throw new Error(`Invalid desktop extension ${label}.`);
  }
}

/** Validate and copy plugin-owned registration data before publication. */
function validateDefinition(value: unknown, expectedId?: string): DesktopExtension {
  if (!record(value) || value.apiVersion !== 1 || !Array.isArray(value.views)) {
    throw new Error("Desktop extension must export an API version 1 definition with views.");
  }
  identity(value.id, "id");
  if (expectedId !== undefined && value.id !== expectedId) {
    throw new Error(`Desktop extension ${expectedId} exported a different id: ${value.id}.`);
  }
  const ids = new Set<string>();
  const views: DesktopView[] = value.views.map((view: unknown) => {
    if (!record(view)) throw new Error(`Invalid view in desktop extension ${value.id}.`);
    identity(view.id, "view id");
    if (ids.has(view.id)) throw new Error(`Duplicate desktop extension view: ${value.id}/${view.id}.`);
    ids.add(view.id);
    if (typeof view.component !== "function" && !(record(view.component) && "$$typeof" in view.component)) {
      throw new Error(`Desktop extension view ${view.id} requires a React component.`);
    }
    if (view.slot === "workspace.panel") {
      identity(view.title, "panel title");
      return Object.freeze({ id: view.id, slot: view.slot, title: view.title, component: view.component }) as PanelView;
    }
    if (view.slot !== "tool.result" || !record(view.target)) {
      throw new Error(`Unsupported desktop extension slot: ${String(view.slot)}.`);
    }
    const keys = Object.keys(view.target);
    if (keys.length !== 1 || (keys[0] !== "tool" && keys[0] !== "customType")) {
      throw new Error(`Desktop extension view ${view.id} requires one exact tool or customType target.`);
    }
    identity(view.target[keys[0]], "target");
    return Object.freeze({
      id: view.id,
      slot: view.slot,
      target: Object.freeze({ ...view.target }),
      component: view.component,
    }) as ItemView;
  });
  return Object.freeze({ apiVersion: 1, id: value.id, views: Object.freeze(views) });
}

/** The registry is immutable after construction, including plugin-supplied metadata. */
export class DesktopExtensionRegistry {
  readonly extensions: readonly LoadedDesktopExtension[];
  readonly panels: readonly RegisteredDesktopView<PanelView>[];
  private readonly tools = new Map<string, RegisteredDesktopView<ItemView>>();
  private readonly customMessages = new Map<string, RegisteredDesktopView<ItemView>>();

  constructor(extensions: readonly LoadedDesktopExtension[] = []) {
    const ids = new Set<string>();
    const panels: RegisteredDesktopView<PanelView>[] = [];
    this.extensions = Object.freeze(extensions.map((loaded) => {
      const definition = validateDefinition(loaded.definition);
      if (ids.has(definition.id)) throw new Error(`Duplicate desktop extension: ${definition.id}.`);
      ids.add(definition.id);
      const entry = Object.freeze({ ...loaded, definition });
      for (const view of definition.views) {
        const registered = Object.freeze({
          extensionId: definition.id, revision: loaded.revision, css: loaded.css ?? "", view,
        });
        if (view.slot === "workspace.panel") {
          panels.push(registered as RegisteredDesktopView<PanelView>);
        } else {
          const [table, target] = "tool" in view.target
            ? [this.tools, view.target.tool] : [this.customMessages, view.target.customType];
          if (table.has(target)) throw new Error(`Duplicate desktop renderer for ${target}.`);
          table.set(target, registered as RegisteredDesktopView<ItemView>);
        }
      }
      return entry;
    }));
    this.panels = Object.freeze(panels);
  }

  itemView(item: DesktopToolItem): RegisteredDesktopView<ItemView> | undefined {
    const customType = item.customType ?? (typeof item.data?.customType === "string" ? item.data.customType : undefined);
    return customType ? this.customMessages.get(customType) : item.toolName ? this.tools.get(item.toolName) : undefined;
  }

  withoutProject(builtins: readonly LoadedDesktopExtension[] = []): DesktopExtensionRegistry {
    const retained = this.extensions.filter((entry) => entry.scope !== "project");
    const ids = new Set(retained.map((entry) => entry.definition.id));
    return new DesktopExtensionRegistry([...builtins.filter((entry) => !ids.has(entry.definition.id)), ...retained]);
  }
}

const MODULE_INIT_TIMEOUT_MS = 10_000;

/** No resolver or filesystem access belongs here: the native host supplies trusted, bundled ESM. */
export async function importDesktopExtension(source: DesktopExtensionSource): Promise<unknown> {
  const url = URL.createObjectURL(new Blob([source.javascript], { type: "text/javascript" }));
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      import(/* @vite-ignore */ url),
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error(`Desktop extension ${source.id} did not initialize within 10 seconds.`)), MODULE_INIT_TIMEOUT_MS);
      }),
    ]);
  } finally {
    if (timeout !== undefined) clearTimeout(timeout);
    URL.revokeObjectURL(url);
  }
}

/** A failed candidate never mutates the prior generation or its module cache. */
export async function prepareDesktopExtensions(
  sources: readonly DesktopExtensionSource[],
  builtins: readonly LoadedDesktopExtension[],
  previous: DesktopExtensionRegistry,
  importer: DesktopExtensionImporter = importDesktopExtension,
): Promise<DesktopExtensionRegistry> {
  const ids = new Set<string>();
  for (const source of sources) {
    identity(source.id, "package id");
    if (ids.has(source.id)) throw new Error(`Duplicate desktop extension package: ${source.id}.`);
    ids.add(source.id);
  }
  const loaded = await Promise.all(sources.map(async (source): Promise<LoadedDesktopExtension> => {
    const cached = previous.extensions.find((entry) =>
      entry.definition.id === source.id && entry.revision === source.revision && entry.scope === source.scope);
    if (cached) return cached;
    const module = await importer(source);
    const definition = validateDefinition(record(module) ? module.default : undefined, source.id);
    return { definition, revision: source.revision, css: source.css, scope: source.scope };
  }));
  // An installed package replaces the complete bundled definition with the same id.
  // Other identity/target collisions still reject the entire generation.
  return new DesktopExtensionRegistry([...builtins.filter((entry) => !ids.has(entry.definition.id)), ...loaded]);
}

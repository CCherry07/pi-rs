import {
  Component,
  Suspense,
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ErrorInfo,
  type ReactNode,
} from "react";
import type { DesktopItemViewProps, DesktopToolItem, DesktopViewContext } from "@pi-rs/desktop-sdk";
import { DesktopContext } from "./runtime";
import { DesktopExtensionRegistry, prepareDesktopExtensions, type RegisteredDesktopView } from "./registry";
import type {
  DesktopExtensionCatalog,
  DesktopExtensionImporter,
  DesktopViewHost,
  LoadedDesktopExtension,
} from "./types";

const EMPTY_BUILTINS: readonly LoadedDesktopExtension[] = Object.freeze([]);
const EMPTY_REGISTRY = new DesktopExtensionRegistry();

type Generation = { registry: DesktopExtensionRegistry; serial: number };
type ExtensionState = Generation & { loading: boolean; error: string | null; reload(): void };
const ExtensionsContext = createContext<ExtensionState>({
  registry: EMPTY_REGISTRY,
  serial: 0,
  loading: false,
  error: null,
  reload: () => {},
});

export type DesktopExtensionsProviderProps = {
  workspaceKey: string;
  builtins?: readonly LoadedDesktopExtension[];
  loadCatalog?: () => Promise<DesktopExtensionCatalog>;
  reloadKey?: unknown;
  importer?: DesktopExtensionImporter;
  onError?: (message: string) => void;
  children: ReactNode;
};

/** Workspace changes immediately unmount old views, including during a pending catalog read. */
export function DesktopExtensionsProvider(props: DesktopExtensionsProviderProps) {
  return <WorkspaceExtensions key={props.workspaceKey} {...props} />;
}

function WorkspaceExtensions({
  builtins = EMPTY_BUILTINS, loadCatalog, reloadKey, importer, onError, children,
}: DesktopExtensionsProviderProps) {
  const [generation, setGeneration] = useState<Generation>(() => ({ registry: new DesktopExtensionRegistry(builtins), serial: 0 }));
  const current = useRef(generation);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const request = useRef(0);
  const reportError = useRef(onError);
  reportError.current = onError;

  useEffect(() => {
    const requestId = ++request.current;
    let disposed = false;
    const isCurrent = () => !disposed && request.current === requestId;
    const publish = (registry: DesktopExtensionRegistry) => {
      const next = { registry, serial: current.current.serial + 1 };
      current.current = next;
      setGeneration(next);
    };
    if (!loadCatalog) {
      publish(new DesktopExtensionRegistry(builtins));
      return () => { disposed = true; };
    }
    setLoading(true);
    setError(null);
    void loadCatalog().then(async (catalog) => {
      if (!isCurrent()) return;
      // A trust revocation is effective even if another package makes the next catalog invalid.
      if (!catalog.projectTrusted && current.current.registry.extensions.some((entry) => entry.scope === "project")) {
        publish(current.current.registry.withoutProject(builtins));
      }
      if (catalog.error) throw new Error(catalog.error);
      const sources = catalog.projectTrusted ? catalog.extensions : catalog.extensions.filter((entry) => entry.scope !== "project");
      const candidate = await prepareDesktopExtensions(sources, builtins, current.current.registry, importer);
      if (isCurrent()) publish(candidate);
    }).catch((cause: unknown) => {
      if (!isCurrent()) return;
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      reportError.current?.(message);
    }).finally(() => {
      if (isCurrent()) setLoading(false);
    });
    return () => { disposed = true; };
  }, [builtins, loadCatalog, reloadKey, importer, attempt]);

  const reload = useCallback(() => setAttempt((value) => value + 1), []);
  const value = useMemo(() => ({ ...generation, loading, error, reload }), [generation, loading, error, reload]);
  return <ExtensionsContext.Provider value={value}>{children}</ExtensionsContext.Provider>;
}

export function useDesktopExtensions() {
  const { loading, error, reload } = useContext(ExtensionsContext);
  return { loading, error, reload };
}

/** Registered rich results remain standalone rows instead of joining generic tool groups. */
export function useDesktopItemMatcher(): (item: DesktopToolItem) => boolean {
  const { registry } = useContext(ExtensionsContext);
  return useCallback((item: DesktopToolItem) => registry.itemView(item) !== undefined, [registry]);
}

type BoundaryProps = { fallback: ReactNode; children: ReactNode; onError?: (message: string) => void };

class ViewBoundary extends Component<BoundaryProps, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() { return { failed: true }; }

  componentDidCatch(error: Error, _info: ErrorInfo) { this.props.onError?.(error.message); }

  render() { return this.state.failed ? this.props.fallback : this.props.children; }
}

function ViewScope({
  host, extensionId, css, children,
}: { host: DesktopViewHost; extensionId: string; css: string; children: ReactNode }) {
  const [controller, setController] = useState<AbortController | null>(null);
  // Create in the effect, not a state initializer: StrictMode tears down and re-runs effects.
  useLayoutEffect(() => {
    const next = new AbortController();
    setController(next);
    return () => { next.abort(); };
  }, []);
  const context = useMemo<DesktopViewContext | null>(() => controller && ({
    pluginId: extensionId,
    workspaceId: host.workspaceId,
    threadId: host.threadId,
    locale: host.locale,
    readOnly: host.readOnly,
    signal: controller.signal,
    widgets: host.widgets,
    runCommand: async (name, args) => {
      if (controller.signal.aborted) throw new Error("This desktop extension view is no longer active.");
      if (host.readOnly) throw new Error("Commands are unavailable in a read-only conversation.");
      await host.runCommand(name, args);
    },
    renderSession: host.renderSession,
    sessionStatus: host.sessionStatus,
  }), [controller, host, extensionId]);

  if (!context) return null;
  return (
    <DesktopContext.Provider value={{ context, renderMarkdown: host.renderMarkdown }}>
      {css && <style data-desktop-extension={extensionId}>{css}</style>}
      {children}
    </DesktopContext.Provider>
  );
}

function ExtensionView({
  host, registered, serial, fallback, children, onError,
}: {
  host: DesktopViewHost;
  registered: RegisteredDesktopView;
  serial: number;
  fallback: ReactNode;
  children: ReactNode;
  onError?: (message: string) => void;
}) {
  // Views retire on a generation/session transition; live widget updates preserve React state.
  const key = `${serial}:${registered.extensionId}:${registered.view.id}:${host.workspaceId}:${host.threadId}:${host.readOnly}`;
  return (
    <ViewBoundary key={key} fallback={fallback} onError={onError}>
      <Suspense fallback={fallback}>
        <ViewScope host={host} extensionId={registered.extensionId} css={registered.css}>
          {children}
        </ViewScope>
      </Suspense>
    </ViewBoundary>
  );
}

export function DesktopItemView({
  host, item, expanded, onToggle, fallback, onError,
}: DesktopItemViewProps & { host: DesktopViewHost; fallback: ReactNode; onError?: (message: string) => void }) {
  const { registry, serial } = useContext(ExtensionsContext);
  const registered = registry.itemView(item);
  // Frozen inherited snapshots retain their saved transcript instead of opening live plugin UI.
  if (!registered || host.frozen) return fallback;
  const Renderer = registered.view.component;
  return (
    <ExtensionView host={host} registered={registered} serial={serial} fallback={fallback} onError={onError}>
      <Renderer item={item} expanded={expanded} onToggle={onToggle} />
    </ExtensionView>
  );
}

function WorkspacePanel({
  host, registered, serial,
}: {
  host: DesktopViewHost;
  registered: RegisteredDesktopView<Extract<LoadedDesktopExtension["definition"]["views"][number], { slot: "workspace.panel" }>>;
  serial: number;
}) {
  const [expanded, setExpanded] = useState(false);
  const Renderer = registered.view.component;
  return (
    <details className="pi-extension-panel" onToggle={(event) => setExpanded(event.currentTarget.open)}>
      <summary>{registered.view.title}</summary>
      {expanded && (
        <ExtensionView
          host={host}
          registered={registered}
          serial={serial}
          fallback={<div role="alert">{host.locale.startsWith("zh") ? "插件视图暂时不可用。" : "Extension view is unavailable."}</div>}
        >
          <Renderer />
        </ExtensionView>
      )}
    </details>
  );
}

export function DesktopWorkspacePanels({ host }: { host: DesktopViewHost }) {
  const { registry, serial } = useContext(ExtensionsContext);
  if (host.readOnly || !registry.panels.length) return null;
  return (
    <div className="pi-extension-panels">
      {registry.panels.map((registered) => (
        <WorkspacePanel
          key={`${registered.extensionId}:${registered.view.id}:${host.workspaceId}:${host.threadId}`}
          host={host}
          registered={registered}
          serial={serial}
        />
      ))}
    </div>
  );
}

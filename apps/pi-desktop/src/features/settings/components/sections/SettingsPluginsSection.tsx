import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ask, open } from "@tauri-apps/plugin-dialog";
import Puzzle from "lucide-react/dist/esm/icons/puzzle";
import RefreshCw from "lucide-react/dist/esm/icons/refresh-cw";
import ArrowLeft from "lucide-react/dist/esm/icons/arrow-left";
import Copy from "lucide-react/dist/esm/icons/copy";
import type { WorkspaceInfo } from "@/types";
import { SettingsSection, SettingsSubsection } from "@/features/design-system/components/settings/SettingsPrimitives";
import { operatePlugins, readPlugins, readPluginRuntime, type PluginLibrarySnapshot, type PluginOperation, type PluginRuntimeSnapshot } from "@/services/plugins";

export type PluginSessionContext = {
  workspaceId: string;
  workspaceName: string;
  threadId: string | null;
  isProcessing: boolean;
  reload: () => Promise<void>;
};
type Props = {
  projects: WorkspaceInfo[];
  onDirtyChange: (dirty: boolean) => void;
  onBusyChange?: (busy: boolean) => void;
  session?: PluginSessionContext;
};

export function SettingsPluginsSection({ projects, onDirtyChange, onBusyChange, session }: Props) {
  const { t } = useTranslation("settings");
  const [workspaceId, setWorkspaceId] = useState<string | null>(null);
  const [snapshot, setSnapshot] = useState<{ scope: string | null; value: PluginLibrarySnapshot } | null>(null);
  const [runtime, setRuntime] = useState<PluginRuntimeSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [runtimeLoading, setRuntimeLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [runtimeError, setRuntimeError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [changed, setChanged] = useState<Set<string | null>>(new Set());
  const [refresh, setRefresh] = useState(0);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);
  const [source, setSource] = useState("");
  const [version, setVersion] = useState("");
  const [registry, setRegistry] = useState("");
  const [accepted, setAccepted] = useState(false);
  const mounted = useRef(false);
  const operationPending = useRef(false);
  const confirmationPending = useRef(false);
  const readRequest = useRef(0);
  const dirty = installing && Boolean(source || version || registry || accepted);
  const library = snapshot?.scope === workspaceId ? snapshot.value : null;
  // A configuration scope is not a runtime selector. Never reuse the current thread for another project.
  const target = session && (workspaceId === null || workspaceId === session.workspaceId) ? session : undefined;
  const targetWorkspace = target?.workspaceId;
  const targetThread = target?.threadId;
  const observed = targetThread && runtime && runtime.workspaceId === targetWorkspace && runtime.threadId === targetThread ? runtime : null;
  const currentContext = useRef({ workspaceId, targetWorkspace, targetThread });
  currentContext.current = { workspaceId, targetWorkspace, targetThread };
  const writable = Boolean(library?.writable && !loading && !busy);
  const detail = library?.plugins.find(plugin => plugin.id === selected);

  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => { onDirtyChange(dirty); return () => onDirtyChange(false); }, [dirty, onDirtyChange]);
  useEffect(() => { onBusyChange?.(busy); return () => onBusyChange?.(false); }, [busy, onBusyChange]);

  useEffect(() => {
    const request = ++readRequest.current;
    let disposed = false;
    setLoading(true);
    setError(null);
    void readPlugins(workspaceId).then(value => {
      if (!disposed && readRequest.current === request) setSnapshot({ scope: workspaceId, value });
    }).catch((reason: unknown) => {
      if (!disposed && readRequest.current === request) {
        setSnapshot(null);
        setError(String(reason));
      }
    }).finally(() => {
      if (!disposed && readRequest.current === request) setLoading(false);
    });
    return () => { disposed = true; };
  }, [workspaceId, refresh]);

  useEffect(() => {
    let disposed = false;
    setRuntime(null);
    setRuntimeError(null);
    setRuntimeLoading(Boolean(targetWorkspace && targetThread));
    if (targetWorkspace && targetThread) {
      void readPluginRuntime(targetWorkspace, targetThread).then(value => {
        if (!disposed) setRuntime(value);
      }).catch((reason: unknown) => {
        if (!disposed) setRuntimeError(String(reason));
      }).finally(() => { if (!disposed) setRuntimeLoading(false); });
    }
    return () => { disposed = true; };
  }, [targetWorkspace, targetThread, workspaceId, refresh]);

  async function discard(next: () => void) {
    if (operationPending.current || confirmationPending.current) return;
    confirmationPending.current = true;
    try {
      if (!dirty || await ask(t("plugins.discard"), { title: t("plugins.title"), kind: "warning" })) {
        if (mounted.current) next();
      }
    } finally { confirmationPending.current = false; }
  }
  function clearDraft() { setInstalling(false); setSource(""); setVersion(""); setAccepted(false); }
  async function mutate(operation: PluginOperation) {
    if (operationPending.current || confirmationPending.current || !writable) return;
    operationPending.current = true;
    setBusy(true); setError(null); setNotice(null);
    const scope = workspaceId;
    try {
      if (operation.kind === "remove" && !await ask(t("plugins.removeConfirm", { id: operation.id, scope: scope ? projects.find(project => project.id === scope)?.name ?? scope : t("plugins.global") }), { title: t("plugins.remove"), kind: "warning" })) return;
      if (!mounted.current || currentContext.current.workspaceId !== scope) return;
      // An operation can commit before its response fails. Do not claim unchanged state on rejection.
      setChanged(previous => new Set(previous).add(scope));
      const value = await operatePlugins(scope, operation);
      if (!mounted.current || currentContext.current.workspaceId !== scope) return;
      ++readRequest.current;
      setSnapshot({ scope, value });
      setNotice(t("plugins.operationSaved"));
      if (operation.kind === "install") clearDraft();
      if (operation.kind === "remove") setSelected(null);
    } catch (reason) {
      if (mounted.current && currentContext.current.workspaceId === scope) {
        setSnapshot(null);
        setError(`${String(reason)}\n${t("plugins.checkState")}`);
      }
    } finally {
      operationPending.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  async function reload() {
    if (!target || target.isProcessing || operationPending.current || confirmationPending.current || loading || (workspaceId !== null && !library?.projectTrusted)) return;
    operationPending.current = true; setBusy(true); setError(null); setNotice(null);
    const context = currentContext.current;
    try {
      if (!await ask(t("plugins.reloadConfirm", { name: target.workspaceName }), { title: t("plugins.reload"), kind: "warning" })) return;
      if (!mounted.current || currentContext.current.workspaceId !== context.workspaceId || currentContext.current.targetWorkspace !== context.targetWorkspace || currentContext.current.targetThread !== context.targetThread) return;
      await target.reload();
      if (!mounted.current || currentContext.current.workspaceId !== context.workspaceId || currentContext.current.targetWorkspace !== context.targetWorkspace || currentContext.current.targetThread !== context.targetThread) return;
      setChanged(previous => { const next = new Set(previous); next.delete(workspaceId); return next; });
      setNotice(t("plugins.reloaded"));
      setRefresh(value => value + 1);
    } catch (reason) {
      if (mounted.current && currentContext.current.workspaceId === context.workspaceId && currentContext.current.targetWorkspace === context.targetWorkspace && currentContext.current.targetThread === context.targetThread) setError(`${t("plugins.reloadFailed")}\n${String(reason)}`);
    } finally { operationPending.current = false; if (mounted.current) setBusy(false); }
  }
  async function chooseFolder() {
    if (operationPending.current || confirmationPending.current) return;
    operationPending.current = true; setBusy(true);
    try {
      const value = await open({ directory: true, multiple: false, title: t("plugins.chooseFolder") });
      if (mounted.current && typeof value === "string") { setSource(value); setAccepted(false); }
    } catch (reason) { if (mounted.current) setError(String(reason)); }
    finally { operationPending.current = false; if (mounted.current) setBusy(false); }
  }
  async function copy(value: string) {
    try { await navigator.clipboard.writeText(value); if (mounted.current) setNotice(t("plugins.copied")); }
    catch (reason) { if (mounted.current) setError(String(reason)); }
  }
  const rows = (library?.plugins ?? []).filter(plugin =>
    `${plugin.id} ${plugin.source}`.toLowerCase().includes(query.toLowerCase()) && (!kind || plugin.installed?.kind === kind));
  const diagnostics = [error, runtimeError, ...(library?.diagnostics ?? [])].filter(Boolean).join("\n");

  return <SettingsSection title={t("plugins.title")} subtitle={t("plugins.subtitle")} className="settings-plugins">
    <div className="settings-plugins-toolbar">
      <select className="settings-select" aria-label={t("plugins.scope")} value={workspaceId ?? ""} disabled={busy} onChange={event => {
        const next = event.target.value || null;
        void discard(() => { setWorkspaceId(next); setSelected(null); clearDraft(); setRegistry(""); setNotice(null); });
      }}>
        <option value="">{t("plugins.global")}</option>
        {projects.map(project => <option key={project.id} value={project.id}>{project.name}</option>)}
      </select>
      <button className="ghost settings-button-compact" aria-label={t("plugins.refresh")} title={t("plugins.refresh")} disabled={busy || loading} onClick={() => setRefresh(value => value + 1)}><RefreshCw aria-hidden /></button>
      <button className="ghost settings-button-compact" disabled={!writable || installing} onClick={() => void mutate({ kind: "sync", ...(registry.trim() ? { registry: registry.trim() } : {}) })}>{t("plugins.sync")}</button>
      <button className="primary settings-button-compact" disabled={!writable || installing} onClick={() => { setInstalling(true); setSelected(null); setNotice(null); }}>{t("plugins.install")}</button>
    </div>
    <p className="settings-help">{t("plugins.scopeHint")}</p>
    {library && <code className="settings-plugins-path">{library.path}</code>}
    {workspaceId && library && !library.projectTrusted && <p role="alert" className="settings-error">{t("plugins.untrusted")}</p>}
    {(changed.has(workspaceId) || library?.intentCurrent === false) && <p role="status" className="settings-plugins-notice">{t("plugins.changed")}</p>}
    {notice && <p role="status" className="settings-help">{notice}</p>}
    {diagnostics && <div role="alert" className="settings-error settings-plugins-diagnostics">{diagnostics}<button className="ghost settings-button-compact" onClick={() => void copy(diagnostics)}><Copy aria-hidden />{t("plugins.copyDiagnostics")}</button></div>}
    {(loading || busy) && <p role="status" className="settings-help">{t("plugins.working")}</p>}

    <div className="settings-plugins-runtime">
      <SettingsSubsection title={t("plugins.sessionTitle")} subtitle={t("plugins.inventoryHint")} />
      {target && <p className="settings-help">{target.workspaceName} · {targetThread ?? t("plugins.preparedDraft")}</p>}
      {!target && <p className="settings-help">{t("plugins.noContext")}</p>}
      {runtimeLoading ? <p role="status" className="settings-help">{t("plugins.observing")}</p> : observed ? <>
        <p className="settings-help">{t("plugins.observedIds")}</p>
        {observed.configuredNativePluginIds.length ? <ul>{observed.configuredNativePluginIds.map(id => <li key={id}><code>{id}</code></li>)}</ul> : <p className="settings-help">{t("plugins.noRuntimePlugins")}</p>}
      </> : <p className="settings-help">{t("plugins.unobserved")}</p>}
      {target && <button className="ghost settings-button-compact" disabled={busy || loading || target.isProcessing || dirty || (workspaceId !== null && !library?.projectTrusted)} onClick={() => void reload()}>{t(targetThread ? "plugins.reload" : "plugins.reloadDraft")}</button>}
    </div>

    <p className="settings-help">{t("plugins.nativeWarning")}</p>
    {installing ? <form className="settings-plugins-form" onSubmit={event => { event.preventDefault(); if (accepted && source.trim()) void mutate({ kind: "install", source: source.trim(), ...(version.trim() ? { version: version.trim() } : {}), ...(registry.trim() ? { registry: registry.trim() } : {}) }); }}>
      <SettingsSubsection title={t("plugins.install")} />
      <label>{t("plugins.source")}<input className="settings-input" value={source} disabled={busy} onChange={event => { setSource(event.target.value); setAccepted(false); }} placeholder="github:owner/repository@tag" /></label>
      <button type="button" className="ghost settings-button-compact" disabled={busy} onClick={() => void chooseFolder()}>{t("plugins.chooseFolder")}</button>
      <p className="settings-help">{t("plugins.sourcesHint")}</p>
      <label>{t("plugins.versionConstraint")}<input className="settings-input" value={version} disabled={busy} onChange={event => { setVersion(event.target.value); setAccepted(false); }} placeholder="^1.0" /></label>
      <label>{t("plugins.registry")}<input className="settings-input" value={registry} disabled={busy} onChange={event => { setRegistry(event.target.value); setAccepted(false); }} placeholder="https://example.com/index.json" /></label>
      <p className="settings-help">{t("plugins.registryHint")}</p>
      <label className="settings-plugins-consent"><input type="checkbox" checked={accepted} disabled={busy} onChange={event => setAccepted(event.target.checked)} />{t("plugins.acceptNative")}</label>
      <div className="settings-field-actions">
        <button type="button" className="ghost settings-button-compact" disabled={busy} onClick={() => void discard(clearDraft)}>{t("plugins.cancel")}</button>
        <button type="submit" className="primary settings-button-compact" disabled={!writable || !source.trim() || !accepted}>{t("plugins.confirmInstall")}</button>
      </div>
    </form> : detail ? <div className="settings-plugins-detail">
      <button className="ghost settings-button-compact" onClick={() => setSelected(null)}><ArrowLeft aria-hidden />{t("plugins.back")}</button>
      <SettingsSubsection title={detail.id} />
      <dl className="settings-plugins-metadata">
        <dt>{t("plugins.configured")}</dt><dd>{t(detail.configured ? "plugins.yes" : "plugins.no")}</dd>
        <dt>{t("plugins.installStatus")}</dt><dd>{t(detail.installed ? "plugins.installed" : "plugins.notInstalled")}</dd>
        <dt>{t("plugins.source")}</dt><dd>{detail.source}<button className="ghost settings-button-compact" aria-label={t("plugins.copySource")} onClick={() => void copy(detail.source)}><Copy aria-hidden /></button></dd>
        <dt>{t("plugins.versionConstraint")}</dt><dd>{detail.requestedVersion ?? "—"}</dd>
        {detail.installed && <>
          <dt>{t("plugins.version")}</dt><dd>{detail.installed.version}</dd>
          <dt>{t("plugins.type")}</dt><dd>{detail.installed.kind}</dd>
          <dt>{t("plugins.target")}</dt><dd>{detail.installed.target}</dd>
          <dt>{t("plugins.resolvedSource")}</dt><dd>{detail.installed.source}</dd>
          <dt>SHA-256</dt><dd>{detail.installed.sha256}<button className="ghost settings-button-compact" aria-label={t("plugins.copyChecksum")} onClick={() => void copy(detail.installed!.sha256)}><Copy aria-hidden /></button></dd>
        </>}
        <dt>plugins.lock</dt><dd>{library?.lockPath}</dd>
      </dl>
      <p className="settings-help">{t("plugins.integrityHint")}</p>
      <p className="settings-help">{t("plugins.removeHint")}</p>
      <button className="ghost danger settings-button-compact" disabled={!writable || !detail.configured || !detail.installed} onClick={() => void mutate({ kind: "remove", id: detail.id })}>{t("plugins.remove")}</button>
    </div> : <>
      <div className="settings-plugins-toolbar">
        <input className="settings-input settings-plugins-search" aria-label={t("plugins.search")} placeholder={t("plugins.search")} value={query} onChange={event => setQuery(event.target.value)} />
        <select className="settings-select" aria-label={t("plugins.type")} value={kind} onChange={event => setKind(event.target.value)}>
          <option value="">{t("plugins.allTypes")}</option><option value="agent">Agent</option><option value="provider">Provider</option><option value="session">Session</option>
        </select>
      </div>
      {!loading && library && rows.length === 0 && <p className="settings-empty">{t("plugins.empty")}</p>}
      <div className="settings-archived-list">
        {rows.map(plugin => <div className="settings-archived-row" key={plugin.id}>
          <div className="settings-archived-info">
            <button className="settings-skills-name" onClick={() => setSelected(plugin.id)}><Puzzle aria-hidden />{plugin.id}</button>
            <p className="settings-help">{plugin.installed ? `${plugin.installed.kind} · ${plugin.installed.version} · ${t("plugins.installed")}` : t("plugins.notInstalled")}</p>
            <div className="settings-archived-path" title={plugin.source}>{plugin.source}</div>
            {!plugin.configured && <p className="settings-help">{t("plugins.notConfigured")}</p>}
          </div>
        </div>)}
      </div>
      <details className="settings-plugins-advanced"><summary>{t("plugins.syncSettings")}</summary>
        <label>{t("plugins.registry")}<input className="settings-input" value={registry} disabled={busy} onChange={event => { setRegistry(event.target.value); setAccepted(false); }} placeholder="https://example.com/index.json" /></label>
        <p className="settings-help">{t("plugins.registryHint")}</p>
        <p className="settings-help">{t("plugins.syncHint")}</p>
      </details>
    </>}
  </SettingsSection>;
}

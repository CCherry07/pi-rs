import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ask } from "@tauri-apps/plugin-dialog";
import Plus from "lucide-react/dist/esm/icons/plus";
import RefreshCw from "lucide-react/dist/esm/icons/refresh-cw";
import Plug from "lucide-react/dist/esm/icons/plug";
import type { WorkspaceInfo } from "@/types";
import { SettingsSection, SettingsToggleSwitch } from "@/features/design-system/components/settings/SettingsPrimitives";
import { readMcpConfig, saveMcpConfig, testMcpConnection, type McpDocument, type McpScope } from "@/services/tauri";

type Props = { projects: WorkspaceInfo[]; onDirtyChange: (dirty: boolean) => void };
type Server = Record<string, unknown>;
type Config = Record<string, unknown> & { mcpServers: Record<string, Server> };
function parse(content: string): Config | null {
  try {
    const value: unknown = JSON.parse(content);
    if (!value || typeof value !== "object" || !("mcpServers" in value)) return null;
    const servers = value.mcpServers;
    if (!servers || typeof servers !== "object" || Array.isArray(servers) || Object.values(servers).some(server => !server || typeof server !== "object" || Array.isArray(server))) return null;
    return value as Config;
  } catch { return null; }
}

export function SettingsMcpSection({ projects, onDirtyChange }: Props) {
  const { t } = useTranslation("settings");
  const [workspaceId, setWorkspaceId] = useState<string | null>(null);
  const scope: McpScope = workspaceId ? "project" : "global";
  const [document, setDocument] = useState<McpDocument | null>(null);
  const [content, setContent] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [raw, setRaw] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const request = useRef(0);
  const dirty = document !== null && content !== document.content;
  const config = parse(content);
  const server = selected ? config?.mcpServers[selected] : undefined;
  const transport = server?.type ?? (server?.url !== undefined ? "http" : "stdio");

  useEffect(() => { onDirtyChange(dirty); return () => onDirtyChange(false); }, [dirty, onDirtyChange]);
  useEffect(() => {
    const id = ++request.current;
    setBusy(true); setDocument(null); setError(null); setNotice(null); setSelected(null);
    void readMcpConfig(workspaceId, scope).then(next => {
      if (request.current !== id) return;
      setDocument(next); setContent(next.content); setRaw(!parse(next.content));
    }).catch((reason: unknown) => { if (request.current === id) setError(String(reason)); })
      .finally(() => { if (request.current === id) setBusy(false); });
    return () => { request.current = id + 1; };
  }, [workspaceId, scope]);

  async function discard() { return !dirty || await ask(t("mcp.discard"), { title: t("mcp.title"), kind: "warning" }); }
  async function run(action: () => Promise<void>) {
    const id = ++request.current;
    setBusy(true); setError(null); setNotice(null);
    try { await action(); } catch (reason) { if (request.current === id) setError(String(reason)); }
    finally { if (request.current === id) setBusy(false); }
  }
  function update(next: Config) { setContent(JSON.stringify(next, null, 2) + "\n"); setNotice(null); }
  function updateServer(name: string, patch: Server) {
    if (config) update({ ...config, mcpServers: { ...config.mcpServers, [name]: { ...config.mcpServers[name], ...patch } } });
  }
  function add() {
    if (!config) return;
    let name = "new-server";
    let index = 2;
    while (config.mcpServers[name]) name = `new-server-${index++}`;
    update({ ...config, mcpServers: { ...config.mcpServers, [name]: { type: "http", url: "https://example.com/mcp", enabled: false } } });
    setSelected(name); setRaw(false);
  }
  async function save() {
    if (!document) return;
    await run(async () => {
      const next = await saveMcpConfig(workspaceId, scope, document.revision, content);
      setDocument(next); setContent(next.content); setNotice(t("mcp.saved"));
    });
  }
  async function refresh() {
    if (!(await discard())) return;
    await run(async () => {
      const next = await readMcpConfig(workspaceId, scope);
      setDocument(next); setContent(next.content); setSelected(null); setRaw(!parse(next.content));
    });
  }
  async function test(name: string) {
    await run(async () => {
      const tools = await testMcpConnection(workspaceId, name);
      setNotice(t("mcp.testSuccess", { count: tools.length }) + (tools.length ? `\n${tools.join("\n")}` : ""));
    });
  }

  return <SettingsSection title={t("mcp.title")} subtitle={t("mcp.subtitle")} className="settings-mcp">
    <div className="settings-mcp-toolbar">
      <select className="settings-select" aria-label={t("mcp.scope")} value={workspaceId ?? ""} disabled={busy} onChange={event => {
        const next = event.target.value || null;
        void discard().then(confirmed => { if (confirmed) setWorkspaceId(next); });
      }}>
        <option value="">{t("mcp.global")}</option>
        {projects.map(project => <option key={project.id} value={project.id}>{project.name}</option>)}
      </select>
      <span className="settings-mcp-toolbar-spacer" />
      <button type="button" className="ghost settings-button-compact settings-mcp-icon" aria-label={t("mcp.refresh")} title={t("mcp.refresh")} disabled={busy} onClick={() => void refresh()}><RefreshCw aria-hidden /></button>
      {document && <button type="button" className="primary settings-button-compact" disabled={busy || !dirty} onClick={() => void save()}>{t("mcp.save")}</button>}
    </div>
    <p className="settings-help">{t("mcp.reloadHint")}</p>
    {document && <code className="settings-mcp-path">{document.path}</code>}
    {error && <div role="alert" className="settings-error">{error}</div>}
    {document?.diagnostic && !dirty && <div role="alert" className="settings-error">{document.diagnostic}</div>}
    {notice && <div role="status" className="settings-mcp-notice">{notice}</div>}
    {busy && <div role="status" className="settings-help">{t("mcp.working")}</div>}
    {document && <>
      <div className="settings-mcp-toolbar">
        <div className="settings-mcp-tabs" role="tablist" aria-label={t("mcp.view")}>
          <button type="button" role="tab" aria-selected={!raw} disabled={!config || busy} onClick={() => setRaw(false)}>{t("mcp.servers")}</button>
          <button type="button" role="tab" aria-selected={raw} disabled={busy} onClick={() => setRaw(true)}>mcp.json</button>
        </div>
        {!raw && <button type="button" className="ghost settings-button-compact" disabled={busy || !config} onClick={add}><Plus aria-hidden />{t("mcp.add")}</button>}
      </div>
      {raw ? <>
        <textarea className="settings-input settings-mcp-source" aria-label="mcp.json" spellCheck={false} value={content} disabled={busy} onChange={event => setContent(event.target.value)} />
        <p className="settings-help">{t("mcp.environmentHint")}</p>
      </> : <>
        <div className="settings-mcp-list">
          {Object.entries(config?.mcpServers ?? {}).map(([name, item]) => <div key={name} className={`settings-mcp-row ${selected === name ? "is-selected" : ""}`}>
            <button type="button" className="settings-mcp-row-main" disabled={busy} onClick={() => setSelected(selected === name ? null : name)} aria-expanded={selected === name}>
              <Plug aria-hidden /><span><strong>{name}</strong><small>{String(item.type ?? (item.url ? "http" : "stdio"))} · {item.enabled === false ? t("mcp.disabled") : t("mcp.enabled")}</small></span>
            </button>
            <SettingsToggleSwitch pressed={item.enabled !== false} aria-label={t("mcp.toggle", { name })} disabled={busy} onClick={() => updateServer(name, { enabled: item.enabled === false })} />
            <button type="button" className="ghost settings-button-compact" disabled={busy || dirty || item.enabled === false} title={dirty ? t("mcp.saveBeforeTest") : undefined} onClick={() => void test(name)}>{t("mcp.test")}</button>
          </div>)}
          {!Object.keys(config?.mcpServers ?? {}).length && <div className="settings-help settings-mcp-empty">{t("mcp.empty")}</div>}
        </div>
        {server && selected && <fieldset className="settings-mcp-form" disabled={busy}>
          <legend>{selected}</legend>
          <label>{t("mcp.name")}<input className="settings-input" key={selected} defaultValue={selected} onBlur={event => {
            const name = event.target.value.trim();
            if (!config || name === selected) return;
            if (!name || Object.prototype.hasOwnProperty.call(config.mcpServers, name)) { setError(t("mcp.nameConflict")); event.target.value = selected; return; }
            const servers = { ...config.mcpServers }; delete servers[selected]; servers[name] = server;
            update({ ...config, mcpServers: servers }); setSelected(name);
          }} /></label>
          <label>{t("mcp.transport")}<select className="settings-select" value={String(transport)} onChange={event => updateServer(selected, { type: event.target.value })}>
            <option value="http">Streamable HTTP</option><option value="stdio">stdio</option>
          </select></label>
          {transport === "http" ? <label>URL<input className="settings-input" value={String(server.url ?? "")} placeholder="https://example.com/mcp" onChange={event => updateServer(selected, { url: event.target.value })} /></label> : <>
            <label>{t("mcp.command")}<input className="settings-input" value={String(server.command ?? "")} onChange={event => updateServer(selected, { command: event.target.value })} /></label>
            <label>{t("mcp.args")}<textarea className="settings-input" value={Array.isArray(server.args) ? server.args.join("\n") : ""} onChange={event => updateServer(selected, { args: event.target.value ? event.target.value.split("\n") : [] })} /></label>
          </>}
          <p className="settings-help">{t("mcp.advancedHint")}</p>
          <button type="button" className="ghost settings-button-compact" onClick={() => void ask(t("mcp.removeConfirm", { name: selected }), { kind: "warning" }).then(confirmed => {
            if (!confirmed || !config) return;
            const servers = { ...config.mcpServers }; delete servers[selected];
            update({ ...config, mcpServers: servers }); setSelected(null);
          })}>{t("mcp.remove")}</button>
        </fieldset>}
        {document.servers.filter(row => row.scope !== scope && !config?.mcpServers[row.name]).map(row => <div className="settings-mcp-row" key={row.name}>
          <span className="settings-mcp-row-main"><Plug aria-hidden /><span>{row.name}<small>{t("mcp.inherited")} · {row.transport}</small></span></span>
          <button type="button" className="ghost settings-button-compact" disabled={busy || dirty || !row.enabled} onClick={() => void test(row.name)}>{t("mcp.test")}</button>
        </div>)}
        <p className="settings-help">{t("mcp.environmentHint")}</p>
      </>}
    </>}
  </SettingsSection>;
}

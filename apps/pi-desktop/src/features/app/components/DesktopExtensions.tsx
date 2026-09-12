import { useCallback, useEffect, useState, type ReactNode } from "react";
import { DesktopExtensionsProvider, useDesktopExtensions } from "../../extensions/ExtensionHost";
import { bundledDesktopExtensions } from "../../extensions/builtins";
import { getDesktopExtensions } from "@services/tauri";
import { subscribePiEvents } from "@services/events";
import { useTranslation } from "react-i18next";
export function DesktopExtensions({ workspaceId, children }: {
    workspaceId: string | null;
    children: ReactNode;
}) {
    const [revision, setRevision] = useState(0);
    const loadCatalog = useCallback(() => workspaceId ? getDesktopExtensions(workspaceId) : Promise.resolve({ extensions: [], projectTrusted: false }), [workspaceId]);
    useEffect(() => subscribePiEvents(event => {
        const params = event.message.params as {
            generationChanged?: boolean;
        } | undefined;
        if (event.workspace_id === workspaceId && event.message.method === "thread/replaced" && params?.generationChanged === true)
            setRevision(value => value + 1);
    }), [workspaceId]);
    return <DesktopExtensionsProvider workspaceKey={workspaceId ?? "none"} builtins={bundledDesktopExtensions} loadCatalog={loadCatalog} reloadKey={revision}>
    {children}
  </DesktopExtensionsProvider>;
}
export function DesktopExtensionControls() {
    const { i18n } = useTranslation();
    const { loading, error, reload } = useDesktopExtensions();
    const zh = i18n.language.startsWith("zh");
    return <div className="desktop-extension-controls">
    {error && <span role="alert">{error}</span>}
    <button type="button" className="ghost" disabled={loading} onClick={reload}>
      {loading ? (zh ? "正在加载扩展…" : "Loading extensions…") : (zh ? "重新加载桌面扩展" : "Reload desktop extensions")}
    </button>
  </div>;
}

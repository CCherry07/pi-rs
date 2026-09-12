import { useEffect, useState } from "react";
import { subscribePiEvents } from "@services/events";
import { getDesktopWidgets, type DesktopWidgetSnapshot } from "@services/tauri";
export function applyWidget(snapshot: DesktopWidgetSnapshot, key: string, value: unknown, version: number): DesktopWidgetSnapshot {
    if (!Number.isSafeInteger(version) || version <= (snapshot.versions[key] ?? -1))
        return snapshot;
    const widgets = { ...snapshot.widgets };
    if (value === null)
        delete widgets[key];
    else
        widgets[key] = value;
    return { ...snapshot, widgets, versions: { ...snapshot.versions, [key]: version } };
}
/** Durable entry sequence orders live updates against a concurrent history read. */
export function useDesktopWidgets(workspaceId: string | null, threadId: string | null) {
    const [state, setState] = useState<{
        key: string;
        snapshot: DesktopWidgetSnapshot;
    }>({ key: "", snapshot: { widgets: {}, versions: {} } });
    const key = `${workspaceId}:${threadId}`;
    useEffect(() => {
        if (!workspaceId || !threadId)
            return;
        let disposed = false;
        let epoch = 0;
        let hydrating = true;
        let snapshot: DesktopWidgetSnapshot = { widgets: {}, versions: {} };
        let pending: {
            key: string;
            value: unknown;
            version: number;
        }[] = [];
        const publish = () => { if (!disposed)
            setState({ key, snapshot }); };
        const refresh = (generationChanged = false) => {
            if (disposed)
                return;
            const request = ++epoch;
            // Snapshot reads also occur after ordinary commands/history refreshes. Keep
            // their views mounted while hydrating, but retire the command scope at once.
            // A real plugin generation replacement invalidates its presentation too.
            snapshot = generationChanged
                ? { widgets: {}, versions: {} }
                : { ...snapshot, scopeToken: undefined };
            pending = [];
            hydrating = true;
            publish();
            void getDesktopWidgets(workspaceId, threadId).then(result => {
                if (disposed || epoch !== request)
                    return;
                hydrating = false;
                snapshot = result;
                for (const update of pending)
                    snapshot = applyWidget(snapshot, update.key, update.value, update.version);
                pending = [];
                publish();
            }).catch(() => { if (epoch === request) {
                hydrating = false;
                pending = [];
            } });
        };
        const unsubscribe = subscribePiEvents(event => {
            if (event.workspace_id !== workspaceId)
                return;
            const { method, params } = event.message;
            if (!params || typeof params !== "object")
                return;
            const data = params as Record<string, unknown>;
            const thread = data.thread as {
                id?: string;
            } | undefined;
            if ((data.threadId ?? thread?.id) !== threadId)
                return;
            if (method === "thread/replaced") {
                refresh(data.generationChanged === true);
                return;
            }
            if (method !== "thread/widgetUpdated" || typeof data.key !== "string" || typeof data.version !== "number")
                return;
            const update = { key: data.key, value: data.value, version: data.version };
            if (hydrating)
                pending.push(update);
            snapshot = applyWidget(snapshot, update.key, update.value, update.version);
            publish();
        }, { onReady: refresh });
        return () => { disposed = true; epoch++; unsubscribe(); };
    }, [workspaceId, threadId, key]);
    return state.key === key ? state.snapshot : { widgets: {}, versions: {} };
}

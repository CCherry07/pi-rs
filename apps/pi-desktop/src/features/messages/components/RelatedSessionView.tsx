import { useContext, useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { DesktopSessionRef } from "@pi-rs/desktop-sdk";
import type { ConversationItem } from "@/types";
import { ThreadConversationsContext } from "@threads/contexts/ThreadConversations";
import { observeDesktopSession } from "@services/tauri";
type Conversation = {
    threadId: string;
    items: ConversationItem[];
    isThinking: boolean;
    isLoadingMessages: boolean;
    processingStartedAt: number | null;
    lastDurationMs: number | null;
};
type RelatedSessionViewProps = {
    reference: DesktopSessionRef;
    workspaceId: string | null;
    threadId: string | null;
    ancestors: readonly string[];
    renderConversation: (conversation: Conversation) => ReactNode;
};
export function RelatedSessionView(props: RelatedSessionViewProps) {
    return <RelatedSessionContent key={JSON.stringify([props.workspaceId, props.threadId, props.reference])} {...props}/>;
}
function RelatedSessionContent({ reference, workspaceId, threadId, ancestors, renderConversation }: RelatedSessionViewProps) {
    const { t } = useTranslation("messages");
    const source = useContext(ThreadConversationsContext);
    const loadThread = source?.loadThread;
    const [state, setState] = useState<{
        id: string | null;
        loading: boolean;
        error: string | null;
    }>({ id: null, loading: true, error: null });
    const [attempt, setAttempt] = useState(0);
    const refKey = JSON.stringify(reference);
    const ancestorKey = JSON.stringify(ancestors);
    const cycle = !!reference.sessionId && ancestors.includes(reference.sessionId);
    useEffect(() => {
        if (!workspaceId || !threadId || !loadThread || cycle)
            return;
        let disposed = false;
        setState({ id: null, loading: true, error: null });
        void (async () => {
            try {
                const result = await observeDesktopSession(workspaceId, threadId, JSON.parse(refKey));
                if (disposed)
                    return;
                const id = result.thread.id;
                if ((JSON.parse(ancestorKey) as string[]).includes(id))
                    throw new Error(t("sessionView.unavailable"));
                setState({ id, loading: true, error: null });
                await loadThread(workspaceId, id);
                if (!disposed)
                    setState({ id, loading: false, error: null });
            }
            catch (error) {
                if (!disposed)
                    setState(previous => ({ ...previous, loading: false, error: error instanceof Error ? error.message : String(error) }));
            }
        })();
        return () => { disposed = true; };
    }, [workspaceId, threadId, loadThread, refKey, ancestorKey, attempt, cycle, t]);
    if (cycle || !workspaceId || !source || !threadId)
        return <div role="status">{t("sessionView.unavailable")}</div>;
    const id = state.id;
    const status = id ? source.threadStatusById[id] : undefined;
    const items = id ? source.itemsByThread[id] ?? [] : [];
    return <>
    {state.error && <div className="desktop-session-error" role="alert">{t("sessionView.loadFailed")} {state.error}
      <button type="button" className="ghost" onClick={() => setAttempt(value => value + 1)}>{t("sessionView.retry")}</button>
    </div>}
    {id && (!state.error || items.length > 0) && renderConversation({ threadId: id, items, isThinking: status?.isProcessing ?? false,
            isLoadingMessages: state.loading, processingStartedAt: status?.processingStartedAt ?? null, lastDurationMs: status?.lastDurationMs ?? null })}
    {!id && state.loading && <div role="status">{t("sessionView.waiting")}</div>}
  </>;
}

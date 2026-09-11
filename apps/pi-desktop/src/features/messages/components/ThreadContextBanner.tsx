import { useId, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import ChevronDown from "lucide-react/dist/esm/icons/chevron-down";
import type { ConversationItem, ThreadContextInheritance } from "@/types";

type ThreadContextBannerProps = {
  inheritance: ThreadContextInheritance;
  isExpanded: boolean;
  onToggle: () => void;
  renderSnapshot: (items: ConversationItem[]) => ReactNode;
};

/** Provenance is separate from the child's timeline, never read from a live parent. */
export function ThreadContextBanner({
  inheritance,
  isExpanded,
  onToggle,
  renderSnapshot,
}: ThreadContextBannerProps) {
  const { t } = useTranslation("messages");
  const bodyId = useId();
  const { origin, inheritedItems } = inheritance;
  const isFork = origin.mode === "fork";
  return (
    <section className="thread-context-banner" aria-label={t("subagents.contextOrigin")}>
      <div className="thread-context-heading">
        <span className="thread-context-mode">{origin.mode}</span>
        <span>{t(isFork ? "subagents.forkContext" : "subagents.freshContext")}</span>
      </div>
      <div className="thread-context-source">
        {t("subagents.parentSession")} <code>{origin.parentThreadId}</code>
        {origin.parentEntryId && <span title={origin.parentEntryId}> · {t("subagents.forkPoint")} <code>{origin.parentEntryId}</code></span>}
      </div>
      <p className="thread-context-hint">{t("subagents.explicitSyncOnly")}</p>
      {isFork && (
        <>
          <button
            type="button"
            className="ghost thread-context-toggle"
            onClick={onToggle}
            aria-expanded={isExpanded}
            aria-controls={bodyId}
          >
            <ChevronDown size={14} aria-hidden />
            {t(isExpanded ? "subagents.hideInheritedContext" : "subagents.viewInheritedContext")}
          </button>
          <div id={bodyId} hidden={!isExpanded}>
            {isExpanded && (
              <section className="thread-context-snapshot" aria-label={t("subagents.inheritedSnapshot")}>
                <p className="thread-context-hint">{t("subagents.snapshotReadOnly")}</p>
                {inheritedItems === null
                  ? <div className="subagent-chat-notice" role="status">{t("subagents.snapshotUnavailable")}</div>
                  : renderSnapshot(inheritedItems)}
              </section>
            )}
          </div>
        </>
      )}
    </section>
  );
}

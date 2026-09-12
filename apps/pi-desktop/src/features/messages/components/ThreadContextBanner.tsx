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
    <section className="thread-context-banner" aria-label={t("relatedSession.contextOrigin")}>
      <div className="thread-context-heading">
        <span className="thread-context-mode">{origin.mode}</span>
        <span>{t(isFork ? "relatedSession.forkContext" : "relatedSession.freshContext")}</span>
      </div>
      <div className="thread-context-source">
        {t("relatedSession.parentSession")} <code>{origin.parentThreadId}</code>
        {origin.parentEntryId && <span title={origin.parentEntryId}> · {t("relatedSession.forkPoint")} <code>{origin.parentEntryId}</code></span>}
      </div>
      <p className="thread-context-hint">{t("relatedSession.explicitSyncOnly")}</p>
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
            {t(isExpanded ? "relatedSession.hideInheritedContext" : "relatedSession.viewInheritedContext")}
          </button>
          <div id={bodyId} hidden={!isExpanded}>
            {isExpanded && (
              <section className="thread-context-snapshot" aria-label={t("relatedSession.inheritedSnapshot")}>
                <p className="thread-context-hint">{t("relatedSession.snapshotReadOnly")}</p>
                {inheritedItems === null
                  ? <div className="thread-context-hint" role="status">{t("relatedSession.snapshotUnavailable")}</div>
                  : renderSnapshot(inheritedItems)}
              </section>
            )}
          </div>
        </>
      )}
    </section>
  );
}

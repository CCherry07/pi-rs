import { useTranslation } from "react-i18next";
import CornerUpLeft from "lucide-react/dist/esm/icons/corner-up-left";
import Pencil from "lucide-react/dist/esm/icons/pencil";
import X from "lucide-react/dist/esm/icons/x";
import type { QueuedMessage } from "../../../types";

type ComposerQueueProps = {
  queuedMessages: QueuedMessage[];
  pausedReason?: string | null;
  steerAvailable?: boolean;
  onSteerQueued?: (item: QueuedMessage) => void;
  onEditQueued?: (item: QueuedMessage) => void;
  onDeleteQueued?: (id: string) => void;
};

export function ComposerQueue({
  queuedMessages,
  pausedReason = null,
  steerAvailable = false,
  onSteerQueued,
  onEditQueued,
  onDeleteQueued,
}: ComposerQueueProps) {
  const { t } = useTranslation("messages");
  if (queuedMessages.length === 0) {
    return null;
  }

  return (
    <div className="composer-queue">
      <div className="composer-queue-title">{t("composer.queueTitle")}</div>
      {pausedReason ? (
        <div className="composer-queue-hint">{pausedReason}</div>
      ) : null}
      <div className="composer-queue-list">
        {queuedMessages.map((item) => (
          <div key={item.id} className="composer-queue-item">
            <span className="composer-queue-text">
              {item.text ||
                (item.images?.length
                  ? item.images.length === 1
                    ? t("composer.image")
                    : t("composer.images")
                  : "")}
              {item.images?.length
                ? ` · ${t("composer.queuedImage", { count: item.images.length })}`
                : ""}
            </span>
            <QueueActions
              item={item}
              steerAvailable={steerAvailable}
              onSteerQueued={onSteerQueued}
              onEditQueued={onEditQueued}
              onDeleteQueued={onDeleteQueued}
            />
          </div>
        ))}
      </div>
    </div>
  );
}

type QueueActionsProps = {
  item: QueuedMessage;
  steerAvailable: boolean;
  onSteerQueued?: (item: QueuedMessage) => void;
  onEditQueued?: (item: QueuedMessage) => void;
  onDeleteQueued?: (id: string) => void;
};

function QueueActions({
  item,
  steerAvailable,
  onSteerQueued,
  onEditQueued,
  onDeleteQueued,
}: QueueActionsProps) {
  const { t } = useTranslation("messages");
  const { t: tCommon } = useTranslation("common");

  return (
    <div className="composer-queue-actions">
      <button
        type="button"
        className="composer-queue-action composer-queue-action-steer"
        onClick={() => onSteerQueued?.(item)}
        disabled={!steerAvailable || !onSteerQueued}
        aria-label={t("composer.steer")}
        title={t("composer.steer")}
      >
        <CornerUpLeft size={13} aria-hidden />
      </button>
      <button
        type="button"
        className="composer-queue-action"
        onClick={() => onEditQueued?.(item)}
        disabled={!onEditQueued}
        aria-label={tCommon("actions.edit")}
        title={tCommon("actions.edit")}
      >
        <Pencil size={13} aria-hidden />
      </button>
      <button
        type="button"
        className="composer-queue-action composer-queue-action-cancel"
        onClick={() => onDeleteQueued?.(item.id)}
        disabled={!onDeleteQueued}
        aria-label={tCommon("actions.cancel")}
        title={tCommon("actions.cancel")}
      >
        <X size={14} aria-hidden />
      </button>
    </div>
  );
}

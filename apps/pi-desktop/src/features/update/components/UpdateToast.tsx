import ReactMarkdown from "react-markdown";
import { useTranslation } from "react-i18next";
import remarkGfm from "remark-gfm";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { PostUpdateNoticeState, UpdateState } from "../hooks/useUpdater";
import {
  ToastActions,
  ToastBody,
  ToastCard,
  ToastError,
  ToastHeader,
  ToastTitle,
  ToastViewport,
} from "../../design-system/components/toast/ToastPrimitives";
import { formatLocalizedNumber } from "../../../i18n/format";

type UpdateToastProps = {
  state: UpdateState;
  onUpdate: () => void;
  onDismiss: () => void;
  postUpdateNotice?: PostUpdateNoticeState;
  onDismissPostUpdateNotice?: () => void;
};

function formatBytes(value: number) {
  if (!Number.isFinite(value) || value <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB"];
  let size = value;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }
  return `${formatLocalizedNumber(size, {
    maximumFractionDigits: size >= 10 ? 0 : 1,
  })} ${units[unitIndex]}`;
}

export function UpdateToast({
  state,
  onUpdate,
  onDismiss,
  postUpdateNotice = null,
  onDismissPostUpdateNotice,
}: UpdateToastProps) {
  const { t } = useTranslation(["app", "common"]);
  if (postUpdateNotice) {
    return (
      <ToastViewport className="update-toasts" role="region" ariaLive="polite">
        <ToastCard className="update-toast" role="status">
          <ToastHeader className="update-toast-header">
            <ToastTitle className="update-toast-title">{t("app:update.whatsNew")}</ToastTitle>
            <div className="update-toast-version">v{postUpdateNotice.version}</div>
          </ToastHeader>
          {postUpdateNotice.stage === "loading" ? (
            <ToastBody className="update-toast-body">
              {t("app:update.notesLoading")}
            </ToastBody>
          ) : null}
          {postUpdateNotice.stage === "ready" ? (
            <>
              <ToastBody className="update-toast-body">
                {t("app:update.notesReady")}
              </ToastBody>
              <div className="update-toast-notes" role="document">
                <ReactMarkdown
                  remarkPlugins={[remarkGfm]}
                  components={{
                    a: ({ href, children }) => {
                      if (!href) {
                        return <span>{children}</span>;
                      }
                      return (
                        <a
                          href={href}
                          target="_blank"
                          rel="noreferrer"
                          onClick={(event) => {
                            event.preventDefault();
                            void openUrl(href);
                          }}
                        >
                          {children}
                        </a>
                      );
                    },
                  }}
                >
                  {postUpdateNotice.body}
                </ReactMarkdown>
              </div>
            </>
          ) : null}
          {postUpdateNotice.stage === "fallback" ? (
            <ToastBody className="update-toast-body">
              {t("app:update.notesFallback", { version: postUpdateNotice.version })}
            </ToastBody>
          ) : null}
          <ToastActions className="update-toast-actions">
            {postUpdateNotice.stage !== "loading" ? (
              <button
                className="primary"
                onClick={() => {
                  void openUrl(postUpdateNotice.htmlUrl);
                }}
              >
                {t("app:update.viewGitHub")}
              </button>
            ) : null}
            <button
              className="secondary"
              onClick={onDismissPostUpdateNotice ?? onDismiss}
            >
              {t("common:actions.dismiss")}
            </button>
          </ToastActions>
        </ToastCard>
      </ToastViewport>
    );
  }

  if (state.stage === "idle") {
    return null;
  }

  const totalBytes = state.progress?.totalBytes;
  const downloadedBytes = state.progress?.downloadedBytes ?? 0;
  const percent =
    totalBytes && totalBytes > 0
      ? Math.min(100, (downloadedBytes / totalBytes) * 100)
      : null;

  return (
    <ToastViewport className="update-toasts" role="region" ariaLive="polite">
      <ToastCard className="update-toast" role="status">
        <ToastHeader className="update-toast-header">
          <ToastTitle className="update-toast-title">{t("app:update.title")}</ToastTitle>
          {state.version ? (
            <div className="update-toast-version">v{state.version}</div>
          ) : null}
        </ToastHeader>
        {state.stage === "checking" && (
          <ToastBody className="update-toast-body">{t("app:update.checking")}</ToastBody>
        )}
        {state.stage === "available" && (
          <>
            <ToastBody className="update-toast-body">
              {t("app:update.available")}
            </ToastBody>
            <ToastActions className="update-toast-actions">
              <button className="secondary" onClick={onDismiss}>
                {t("app:update.later")}
              </button>
              <button className="primary" onClick={onUpdate}>
                {t("app:update.title")}
              </button>
            </ToastActions>
          </>
        )}
        {state.stage === "latest" && (
          <div className="update-toast-inline">
            <ToastBody className="update-toast-body update-toast-body-inline">
              {t("app:update.latest")}
            </ToastBody>
            <button className="secondary" onClick={onDismiss}>
              {t("common:actions.dismiss")}
            </button>
          </div>
        )}
        {state.stage === "downloading" && (
          <>
            <ToastBody className="update-toast-body">
              {t("app:update.downloading")}
            </ToastBody>
            <div className="update-toast-progress">
              <div className="update-toast-progress-bar">
                <span
                  className="update-toast-progress-fill"
                  style={{ width: percent ? `${percent}%` : "24%" }}
                />
              </div>
              <div className="update-toast-progress-meta">
                {totalBytes
                  ? `${formatBytes(downloadedBytes)} / ${formatBytes(totalBytes)}`
                  : t("app:update.downloaded", { size: formatBytes(downloadedBytes) })}
              </div>
            </div>
          </>
        )}
        {state.stage === "installing" && (
          <ToastBody className="update-toast-body">{t("app:update.installing")}</ToastBody>
        )}
        {state.stage === "restarting" && (
          <ToastBody className="update-toast-body">{t("app:update.restarting")}</ToastBody>
        )}
        {state.stage === "error" && (
          <>
            <ToastBody className="update-toast-body">{t("app:update.failed")}</ToastBody>
            {state.error ? (
              <ToastError className="update-toast-error">{state.error}</ToastError>
            ) : null}
            <ToastActions className="update-toast-actions">
              <button className="secondary" onClick={onDismiss}>
                {t("common:actions.dismiss")}
              </button>
              <button className="primary" onClick={onUpdate}>
                {t("common:actions.retry")}
              </button>
            </ToastActions>
          </>
        )}
      </ToastCard>
    </ToastViewport>
  );
}

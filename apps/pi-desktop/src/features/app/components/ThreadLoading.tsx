import { useTranslation } from "react-i18next";

type ThreadLoadingProps = {
  nested?: boolean;
};

export function ThreadLoading({ nested }: ThreadLoadingProps) {
  const { t } = useTranslation("app");
  return (
    <div
      className={`thread-loading${nested ? " thread-loading-nested" : ""}`}
      aria-label={t("sidebar.thread.loadingAgents")}
    >
      <span className="thread-skeleton thread-skeleton-wide" />
      <span className="thread-skeleton" />
      <span className="thread-skeleton thread-skeleton-short" />
    </div>
  );
}

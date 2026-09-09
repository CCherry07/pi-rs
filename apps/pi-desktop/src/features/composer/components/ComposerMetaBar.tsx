import type { CSSProperties } from "react";
import { BrainCog, Zap } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { ServiceTier, ThreadTokenUsage } from "../../../types";

export function formatTokens(tokens: number): string {
  const normalizedTokens = Math.max(0, tokens);
  const useMillions = normalizedTokens >= 1_000_000;
  const value = normalizedTokens / (useMillions ? 1_000_000 : 1_000);
  const digits = useMillions
    ? 3
    : value < 10 && !Number.isInteger(value)
      ? 1
      : 0;
  const compactValue = value
    .toFixed(digits)
    .replace(/(\.\d*?)0+$/, "$1")
    .replace(/\.$/, "");
  return `${compactValue}${useMillions ? "M" : "k"}`;
}

export function formatTokenUsageLabel(tokenUsage: ThreadTokenUsage | null): string {
  return tokenUsage?.totalTokens === null || tokenUsage?.totalTokens === undefined
    ? "--"
    : formatTokens(tokenUsage.totalTokens);
}

type ComposerMetaBarProps = {
  disabled: boolean;
  models: { id: string; displayName: string; model: string }[];
  selectedModelId: string | null;
  onSelectModel: (id: string) => void;
  reasoningOptions: string[];
  selectedEffort: string | null;
  onSelectEffort: (effort: string) => void;
  selectedServiceTier: ServiceTier | null;
  reasoningSupported: boolean;
  tokenUsage?: ThreadTokenUsage | null;
};

export function ComposerMetaBar({
  disabled,
  models,
  selectedModelId,
  onSelectModel,
  reasoningOptions,
  selectedEffort,
  onSelectEffort,
  selectedServiceTier,
  reasoningSupported,
  tokenUsage = null,
}: ComposerMetaBarProps) {
  const { t } = useTranslation(["messages", "common"]);
  const selectedModel =
    models.find((model) => model.id === selectedModelId) ?? null;
  const selectedModelLabel =
    selectedModel?.displayName || selectedModel?.model || t("composer.noModels");
  const modelSelectStyle = {
    "--composer-model-select-width": `${Math.max(selectedModelLabel.length + 2, 8)}ch`,
  } as CSSProperties;
  const contextWindow = tokenUsage?.modelContextWindow ?? null;
  const contextTokens = tokenUsage?.contextTokens ?? null;
  const contextFreePercent =
    contextWindow && contextWindow > 0 && contextTokens !== null
      ? Math.max(
          0,
          100 -
            Math.min(Math.max((contextTokens / contextWindow) * 100, 0), 100),
        )
      : null;
  const boundedUsedTokens =
    contextWindow && contextTokens !== null
      ? Math.min(Math.max(contextTokens, 0), contextWindow)
      : contextTokens;
  const remainingTokens =
    contextWindow && boundedUsedTokens !== null
      ? Math.max(0, contextWindow - boundedUsedTokens)
      : null;
  const tokenUsageLabel = formatTokenUsageLabel(tokenUsage);
  const tokenUsageTooltip =
    tokenUsage?.totalTokens === null || tokenUsage?.totalTokens === undefined
      ? t("composer.tokenUsageUnknown")
      : t("composer.tokenUsage", { tokens: tokenUsageLabel });
  const contextTooltip =
    boundedUsedTokens !== null && remainingTokens !== null && contextFreePercent !== null
      ? t("composer.contextUsage", {
          used: formatTokens(boundedUsedTokens),
          remaining: formatTokens(remainingTokens),
          percent: Math.round(contextFreePercent),
        })
      : t("composer.contextFreeUnknown");
  return (
    <div className="composer-bar">
      <div className="composer-meta">
        <div className="composer-select-wrap composer-select-wrap--model">
          <span className="composer-icon composer-icon--model" aria-hidden>
            <svg viewBox="0 0 24 24" fill="none">
              <path
                d="M12 4v2"
                stroke="currentColor"
                strokeWidth="1.4"
                strokeLinecap="round"
              />
              <path
                d="M8 7.5h8a2.5 2.5 0 0 1 2.5 2.5v5a2.5 2.5 0 0 1-2.5 2.5H8A2.5 2.5 0 0 1 5.5 15v-5A2.5 2.5 0 0 1 8 7.5Z"
                stroke="currentColor"
                strokeWidth="1.4"
                strokeLinejoin="round"
              />
              <circle cx="9.5" cy="12.5" r="1" fill="currentColor" />
              <circle cx="14.5" cy="12.5" r="1" fill="currentColor" />
              <path
                d="M9.5 15.5h5"
                stroke="currentColor"
                strokeWidth="1.4"
                strokeLinecap="round"
              />
              <path
                d="M5.5 11H4M20 11h-1.5"
                stroke="currentColor"
                strokeWidth="1.4"
                strokeLinecap="round"
              />
            </svg>
          </span>
          <select
            className="composer-select composer-select--model"
            aria-label={t("composer.model")}
            value={selectedModelId ?? ""}
            onChange={(event) => onSelectModel(event.target.value)}
            disabled={disabled}
            style={modelSelectStyle}
          >
            {models.length === 0 && <option value="">{t("composer.noModels")}</option>}
            {models.map((model) => (
              <option key={model.id} value={model.id}>
                {model.displayName || model.model}
              </option>
            ))}
          </select>
          {selectedServiceTier === "fast" && (
            <span
              className="composer-fast-indicator"
              role="status"
              aria-label={t("composer.fastMode")}
              title={t("composer.fastMode")}
            >
              <Zap size={12} strokeWidth={1.8} />
            </span>
          )}
        </div>
        <div className="composer-select-wrap composer-select-wrap--effort">
          <span className="composer-icon composer-icon--effort" aria-hidden>
            <BrainCog size={14} strokeWidth={1.8} />
          </span>
          <select
            className="composer-select composer-select--effort"
            aria-label={t("composer.thinkingMode")}
            value={selectedEffort ?? ""}
            onChange={(event) => onSelectEffort(event.target.value)}
            disabled={disabled || !reasoningSupported}
          >
            {reasoningOptions.length === 0 && (
              <option value="">{t("composer.default")}</option>
            )}
            {reasoningOptions.map((effort) => (
              <option key={effort} value={effort}>
                {t(`common:reasoning.${effort}` as "common:reasoning.low", {
                  defaultValue: effort,
                })}
              </option>
            ))}
          </select>
        </div>
      </div>
      <div className="composer-context">
        <span className="composer-context-label" title={tokenUsageTooltip}>
          {tokenUsageLabel}
        </span>
        <div
          className="composer-context-ring"
          data-tooltip={contextTooltip}
          aria-label={contextTooltip}
          style={
            {
              "--context-free": contextFreePercent ?? 0,
            } as CSSProperties
          }
        >
          <span className="composer-context-value">●</span>
        </div>
      </div>
    </div>
  );
}

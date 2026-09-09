import type { ThreadTokenUsage } from "../types";

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

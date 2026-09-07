import type { UiLocale } from "@/types";

export const DEFAULT_UI_LOCALE = "en" as const;
export const SUPPORTED_UI_LOCALES = ["en", "zh-CN"] as const;

export type ResolvedUiLocale = (typeof SUPPORTED_UI_LOCALES)[number];

const allowedUiLocales = new Set<UiLocale>([
  "system",
  ...SUPPORTED_UI_LOCALES,
]);

export function normalizeUiLocale(value: unknown): UiLocale {
  return typeof value === "string" && allowedUiLocales.has(value as UiLocale)
    ? (value as UiLocale)
    : "system";
}

export function getSystemLanguages(): readonly string[] {
  if (typeof navigator === "undefined") {
    return [];
  }
  if (navigator.languages.length > 0) {
    return navigator.languages;
  }
  return navigator.language ? [navigator.language] : [];
}

export function matchSupportedUiLocale(
  languages: readonly string[],
): ResolvedUiLocale {
  for (const language of languages) {
    const normalized = language.trim().replace(/_/g, "-").toLowerCase();
    if (normalized === "zh" || normalized.startsWith("zh-")) {
      return "zh-CN";
    }
    if (normalized === "en" || normalized.startsWith("en-")) {
      return "en";
    }
  }
  return DEFAULT_UI_LOCALE;
}

export function resolveUiLocale(
  preference: UiLocale,
  systemLanguages: readonly string[] = getSystemLanguages(),
): ResolvedUiLocale {
  return preference === "system"
    ? matchSupportedUiLocale(systemLanguages)
    : preference;
}

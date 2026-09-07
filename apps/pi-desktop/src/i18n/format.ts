import { i18n } from "./index";
import {
  DEFAULT_UI_LOCALE,
  matchSupportedUiLocale,
  type ResolvedUiLocale,
} from "./locale";

export function getFormattingLocale(): ResolvedUiLocale {
  const activeLanguage = i18n.resolvedLanguage || i18n.language;
  return activeLanguage
    ? matchSupportedUiLocale([activeLanguage])
    : DEFAULT_UI_LOCALE;
}

export function formatLocalizedNumber(
  value: number,
  options?: Intl.NumberFormatOptions,
) {
  return new Intl.NumberFormat(getFormattingLocale(), options).format(value);
}

export function formatLocalizedDate(
  value: Date | number,
  options?: Intl.DateTimeFormatOptions,
) {
  return new Intl.DateTimeFormat(getFormattingLocale(), options).format(value);
}

export function formatLocalizedRelativeTime(
  value: number,
  unit: Intl.RelativeTimeFormatUnit,
  options?: Intl.RelativeTimeFormatOptions,
) {
  return new Intl.RelativeTimeFormat(getFormattingLocale(), options).format(
    value,
    unit,
  );
}

import { formatLocalizedRelativeTime } from "@/i18n/format";
import i18n from "@/i18n";

export function formatRelativeTime(timestamp: number) {
  const now = Date.now();
  const diffSeconds = Math.round((timestamp - now) / 1000);
  const absSeconds = Math.abs(diffSeconds);
  const ranges: { unit: Intl.RelativeTimeFormatUnit; seconds: number }[] = [
    { unit: "year", seconds: 60 * 60 * 24 * 365 },
    { unit: "month", seconds: 60 * 60 * 24 * 30 },
    { unit: "week", seconds: 60 * 60 * 24 * 7 },
    { unit: "day", seconds: 60 * 60 * 24 },
    { unit: "hour", seconds: 60 * 60 },
    { unit: "minute", seconds: 60 },
    { unit: "second", seconds: 1 },
  ];
  const range =
    ranges.find((entry) => absSeconds >= entry.seconds) ||
    ranges[ranges.length - 1];
  if (!range || absSeconds < 5) {
    return formatLocalizedRelativeTime(0, "second", { numeric: "auto" });
  }
  const value = Math.round(diffSeconds / range.seconds);
  return formatLocalizedRelativeTime(value, range.unit, {
    numeric: "auto",
    style: "long",
  });
}

export function formatRelativeTimeShort(timestamp: number) {
  const now = Date.now();
  const absSeconds = Math.abs(Math.round((timestamp - now) / 1000));
  if (absSeconds < 60) {
    return i18n.t("relativeShort.now", { ns: "common" });
  }
  if (absSeconds < 60 * 60) {
    return i18n.t("relativeShort.minute", {
      ns: "common",
      count: Math.max(1, Math.round(absSeconds / 60)),
    });
  }
  if (absSeconds < 60 * 60 * 24) {
    return i18n.t("relativeShort.hour", {
      ns: "common",
      count: Math.max(1, Math.round(absSeconds / (60 * 60))),
    });
  }
  if (absSeconds < 60 * 60 * 24 * 7) {
    return i18n.t("relativeShort.day", {
      ns: "common",
      count: Math.max(1, Math.round(absSeconds / (60 * 60 * 24))),
    });
  }
  if (absSeconds < 60 * 60 * 24 * 30) {
    return i18n.t("relativeShort.week", {
      ns: "common",
      count: Math.max(1, Math.round(absSeconds / (60 * 60 * 24 * 7))),
    });
  }
  if (absSeconds < 60 * 60 * 24 * 365) {
    return i18n.t("relativeShort.month", {
      ns: "common",
      count: Math.max(1, Math.round(absSeconds / (60 * 60 * 24 * 30))),
    });
  }
  return i18n.t("relativeShort.year", {
    ns: "common",
    count: Math.max(1, Math.round(absSeconds / (60 * 60 * 24 * 365))),
  });
}

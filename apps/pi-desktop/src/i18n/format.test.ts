import { afterEach, describe, expect, it } from "vitest";
import { i18n } from "./index";
import {
  formatLocalizedDate,
  formatLocalizedNumber,
  formatLocalizedRelativeTime,
  getFormattingLocale,
} from "./format";

describe("localized formatting", () => {
  afterEach(async () => {
    await i18n.changeLanguage("en");
  });

  it("uses the active UI locale for dates, numbers, and relative time", async () => {
    await i18n.changeLanguage("zh-CN");

    expect(getFormattingLocale()).toBe("zh-CN");
    expect(
      formatLocalizedDate(new Date(2026, 0, 20), {
        month: "short",
        day: "numeric",
      }),
    ).toContain("1月20日");
    expect(formatLocalizedNumber(1234)).toBe("1,234");
    expect(
      formatLocalizedRelativeTime(-5, "minute", {
        numeric: "always",
      }),
    ).toBe("5分钟前");
  });
});

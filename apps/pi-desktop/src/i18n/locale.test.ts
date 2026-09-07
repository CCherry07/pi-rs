import { describe, expect, it } from "vitest";
import {
  matchSupportedUiLocale,
  normalizeUiLocale,
  resolveUiLocale,
} from "./locale";

describe("UI locale", () => {
  it("normalizes persisted values", () => {
    expect(normalizeUiLocale("zh-CN")).toBe("zh-CN");
    expect(normalizeUiLocale("en")).toBe("en");
    expect(normalizeUiLocale("unsupported")).toBe("system");
    expect(normalizeUiLocale(null)).toBe("system");
  });

  it("matches language variants", () => {
    expect(matchSupportedUiLocale(["zh-Hans-CN", "en-US"])).toBe("zh-CN");
    expect(matchSupportedUiLocale(["fr-FR", "en_GB"])).toBe("en");
    expect(matchSupportedUiLocale(["fr-FR"])).toBe("en");
  });

  it("honors an explicit preference", () => {
    expect(resolveUiLocale("en", ["zh-CN"])).toBe("en");
    expect(resolveUiLocale("zh-CN", ["en-US"])).toBe("zh-CN");
    expect(resolveUiLocale("system", ["zh-CN"])).toBe("zh-CN");
  });
});

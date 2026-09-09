import { describe, expect, it } from "vitest";
import { formatTokens, formatTokenUsageLabel } from "./ComposerMetaBar";

describe("formatTokens", () => {
  it("formats token counts with compact k and M units", () => {
    expect(formatTokens(0)).toBe("0k");
    expect(formatTokens(500)).toBe("0.5k");
    expect(formatTokens(1_250)).toBe("1.3k");
    expect(formatTokens(32_000)).toBe("32k");
    expect(formatTokens(128_000)).toBe("128k");
    expect(formatTokens(1_000_000)).toBe("1M");
    expect(formatTokens(1_001_000)).toBe("1.001M");
    expect(formatTokens(1_250_000)).toBe("1.25M");
    expect(formatTokens(12_000_000)).toBe("12M");
    expect(formatTokens(12_345_000)).toBe("12.345M");
  });
});

describe("formatTokenUsageLabel", () => {
  it("shows cumulative token usage instead of current context usage", () => {
    expect(
      formatTokenUsageLabel({
        totalTokens: 8_500,
        contextTokens: 32_000,
        modelContextWindow: 128_000,
      }),
    ).toBe("8.5k");
  });

  it("keeps unavailable cumulative usage explicit", () => {
    expect(
      formatTokenUsageLabel({
        totalTokens: null,
        contextTokens: 32_000,
        modelContextWindow: 128_000,
      }),
    ).toBe("--");
  });
});

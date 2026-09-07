import { describe, expect, it } from "vitest";
import { formatContextTokens } from "./ComposerMetaBar";

describe("formatContextTokens", () => {
  it("formats context counts with compact decimal k units", () => {
    expect(formatContextTokens(0)).toBe("0k");
    expect(formatContextTokens(500)).toBe("0.5k");
    expect(formatContextTokens(1_250)).toBe("1.3k");
    expect(formatContextTokens(32_000)).toBe("32k");
    expect(formatContextTokens(128_000)).toBe("128k");
  });
});

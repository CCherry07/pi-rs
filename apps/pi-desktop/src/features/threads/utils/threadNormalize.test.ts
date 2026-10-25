import { describe, expect, it } from "vitest";
import {
  normalizeRootPath,
  normalizeTokenUsage,
} from "./threadNormalize";

describe("normalizeTokenUsage", () => {
  it("normalizes a desktop context snapshot", () => {
    expect(
      normalizeTokenUsage({
        totalTokens: 48_000,
        contextTokens: 32_000,
        modelContextWindow: 128_000,
      }),
    ).toEqual({
      totalTokens: 48_000,
      contextTokens: 32_000,
      modelContextWindow: 128_000,
    });
  });

  it("keeps context tokens unknown after compaction", () => {
    expect(
      normalizeTokenUsage({
        totalTokens: 64_000,
        contextTokens: null,
        modelContextWindow: 128_000,
      }),
    ).toEqual({
      totalTokens: 64_000,
      contextTokens: null,
      modelContextWindow: 128_000,
    });
  });
});

describe("normalizeRootPath", () => {
  it("preserves significant leading and trailing whitespace", () => {
    expect(normalizeRootPath(" /tmp/repo ")).toBe(" /tmp/repo ");
  });

  it("normalizes Windows drive-letter paths case-insensitively", () => {
    expect(normalizeRootPath("C:\\Dev\\Repo\\")).toBe("c:/dev/repo");
    expect(normalizeRootPath("c:/Dev/Repo")).toBe("c:/dev/repo");
  });

  it("normalizes UNC paths case-insensitively", () => {
    expect(normalizeRootPath("\\\\SERVER\\Share\\Repo\\")).toBe(
      "//server/share/repo",
    );
  });

  it("strips Windows namespace prefixes from drive-letter paths", () => {
    expect(normalizeRootPath("\\\\?\\C:\\Dev\\Repo\\")).toBe("c:/dev/repo");
    expect(normalizeRootPath("\\\\.\\C:\\Dev\\Repo\\")).toBe("c:/dev/repo");
  });

  it("strips Windows namespace prefixes from UNC paths", () => {
    expect(normalizeRootPath("\\\\?\\UNC\\SERVER\\Share\\Repo\\")).toBe(
      "//server/share/repo",
    );
  });
});

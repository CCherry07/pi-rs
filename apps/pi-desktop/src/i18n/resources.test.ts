import { describe, expect, it } from "vitest";
import { resources } from "./resources";

function translationKeys(value: unknown, prefix = ""): string[] {
  if (typeof value !== "object" || value === null) {
    return [prefix];
  }
  return Object.entries(value).flatMap(([key, child]) =>
    translationKeys(child, prefix ? `${prefix}.${key}` : key),
  );
}

describe("translation resources", () => {
  it("keeps English and Simplified Chinese keys in sync", () => {
    expect(translationKeys(resources["zh-CN"]).sort()).toEqual(
      translationKeys(resources.en).sort(),
    );
  });
});

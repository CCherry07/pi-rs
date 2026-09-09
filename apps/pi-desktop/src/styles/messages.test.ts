import { describe, expect, it, vi } from "vitest";

function ruleBody(stylesheet: string, selector: string) {
  const ruleStart = stylesheet.indexOf(`${selector} {`);
  expect(ruleStart, `missing CSS rule for ${selector}`).toBeGreaterThanOrEqual(0);
  const bodyStart = stylesheet.indexOf("{", ruleStart) + 1;
  const bodyEnd = stylesheet.indexOf("}", bodyStart);
  return stylesheet.slice(bodyStart, bodyEnd);
}

describe("Markdown table layout", () => {
  it("keeps arbitrary table columns content-driven within the message width", async () => {
    const { readFileSync } = await vi.importActual<{
      readFileSync(path: URL, encoding: "utf8"): string;
    }>("node:fs");
    const stylesheet = readFileSync(new URL("./messages.css", import.meta.url), "utf8");
    const tableRule = ruleBody(stylesheet, ".markdown .markdown-table");

    expect(tableRule).toMatch(/width:\s*100%\s*;/);
    expect(tableRule).toMatch(/table-layout:\s*auto\s*;/);
    expect(stylesheet).not.toMatch(
      /\.message \.markdown \.markdown-table (?:th|td):(?:first-child|last-child|nth-child\()/,
    );
  });
});

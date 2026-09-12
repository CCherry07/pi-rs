import { describe, expect, it, vi } from "vitest";

function ruleBody(stylesheet: string, selector: string) {
  const ruleStart = stylesheet.indexOf(`${selector} {`);
  expect(ruleStart, `missing CSS rule for ${selector}`).toBeGreaterThanOrEqual(0);
  const bodyStart = stylesheet.indexOf("{", ruleStart) + 1;
  const bodyEnd = stylesheet.indexOf("}", bodyStart);
  return stylesheet.slice(bodyStart, bodyEnd);
}

describe("Chat overflow layout", () => {
  it("lets wide tables scroll without squeezing arbitrary columns", async () => {
    const { readFileSync } = await vi.importActual<{
      readFileSync(path: URL, encoding: "utf8"): string;
    }>("node:fs");
    const stylesheet = readFileSync(new URL("./messages.css", import.meta.url), "utf8");
    const tableRule = ruleBody(stylesheet, ".markdown .markdown-table");

    expect(tableRule).toMatch(/(?:^|;)\s*width:\s*max-content\s*;/);
    expect(tableRule).toMatch(/min-width:\s*100%\s*;/);
    expect(tableRule).toMatch(/table-layout:\s*auto\s*;/);
    expect(ruleBody(stylesheet, ".markdown .markdown-table-wrap")).toMatch(/overflow-x:\s*auto\s*;/);
    expect(stylesheet).not.toMatch(
      /\.message \.markdown \.markdown-table (?:th|td):(?:first-child|last-child|nth-child\()/,
    );
  });

  it("lets an embedded chat hand vertical scrolling back to its parent at the boundary", async () => {
    const { readFileSync } = await vi.importActual<{
      readFileSync(path: URL, encoding: "utf8"): string;
    }>("node:fs");
    const stylesheet = readFileSync(new URL("./messages.css", import.meta.url), "utf8");
    const embedded = ruleBody(stylesheet, ".messages.messages-embedded");
    expect(embedded).toMatch(/max-height:\s*min\(/);
    expect(embedded).toMatch(/overflow:\s*auto\s*;/);
    expect(embedded).toMatch(/overscroll-behavior-y:\s*auto\s*;/);
    expect(embedded).not.toMatch(/overscroll-behavior:\s*contain\s*;/);
    expect(embedded).toMatch(/--composer-overlay-height:\s*0px\s*;/);
    expect(embedded).toMatch(/flex:\s*none\s*;/);
  });
});

import { describe, expect, it } from "vitest";
import { DIFF_VIEWER_SCROLL_CSS } from "./diffViewerTheme";

describe("diff viewer theme", () => {
  it("does not force hunk separators into the line-number grid flow", () => {
    const staticPositionSelectors = DIFF_VIEWER_SCROLL_CSS.match(
      /([^{}]+)\{\s*position:\s*static\s*!important;\s*}/,
    )?.[1];

    expect(staticPositionSelectors).toBeDefined();
    expect(staticPositionSelectors).not.toContain("[data-separator-wrapper]");
  });
});

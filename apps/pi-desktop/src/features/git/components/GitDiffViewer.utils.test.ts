import { describe, expect, it } from "vitest";
import { buildPierreFileDiff } from "./GitDiffViewer.utils";

const PATCH = [
  "diff --git a/src/demo.ts b/src/demo.ts",
  "index 111..222 100644",
  "--- a/src/demo.ts",
  "+++ b/src/demo.ts",
  "@@ -1,3 +1,3 @@",
  " alpha",
  "-beta",
  "+BETA",
  " gamma",
  "",
].join("\n");

describe("buildPierreFileDiff", () => {
  it("parses a patch into partial file metadata", () => {
    const file = buildPierreFileDiff({
      diff: PATCH,
      displayPath: "src/demo.ts",
    });

    expect(file).toMatchObject({
      name: "src/demo.ts",
      isPartial: true,
      additionLines: ["alpha\n", "BETA\n", "gamma\n"],
      deletionLines: ["alpha\n", "beta\n", "gamma\n"],
    });
    expect(file?.hunks).toHaveLength(1);
  });

  it("uses complete file contents when both sides are available", () => {
    const file = buildPierreFileDiff({
      diff: PATCH,
      displayPath: "src/demo.ts",
      oldLines: ["alpha\n", "beta\n", "gamma\n"],
      newLines: ["alpha\n", "BETA\n", "gamma\n"],
      status: "M",
    });

    expect(file).toMatchObject({
      name: "src/demo.ts",
      isPartial: false,
      additionLines: ["alpha\n", "BETA\n", "gamma\n"],
      deletionLines: ["alpha\n", "beta\n", "gamma\n"],
    });
    expect(file?.hunks).toHaveLength(1);
  });
});

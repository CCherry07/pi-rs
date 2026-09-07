import { build } from "vite";
import { describe, expect, it } from "vitest";

describe("Pierre diff production bundle", () => {
  it("keeps the diffs-container custom-element registration", async () => {
    const result = await build({
      configFile: false,
      logLevel: "silent",
      build: {
        write: false,
        minify: false,
        lib: {
          entry: decodeURIComponent(
            new URL(
              "../test/fixtures/pierreDiffBundleEntry.ts",
              import.meta.url,
            ).pathname,
          ),
          formats: ["es"],
        },
        rollupOptions: {
          external: ["react", "react/jsx-runtime"],
        },
      },
    });

    const outputs = (Array.isArray(result) ? result : [result]).flatMap(
      (entry) => ("output" in entry ? entry.output : []),
    );
    const code = outputs
      .flatMap((output) => (output.type === "chunk" ? [output.code] : []))
      .join("\n");

    expect(code).toContain("customElements.define");
    expect(code).toContain('"diffs-container"');
  });
});

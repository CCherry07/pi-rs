// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Markdown } from "./Markdown";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) =>
    `asset://localhost/${encodeURIComponent(path)}`,
}));

const pixel =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=";

describe("Markdown body images", () => {
  afterEach(cleanup);

  it.each([
    ["/tmp/figure.png", "/tmp/figure.png"],
    ["./images/figure.png", "/repo/images/figure.png"],
    ["images/figure.png", "/repo/images/figure.png"],
    ["../figure.png", "/figure.png"],
    ["</tmp/图 表.png>", "/tmp/图 表.png"],
    ["/tmp/figure%23one.png", "/tmp/figure#one.png"],
    ["file:///tmp/figure.png", "/tmp/figure.png"],
    ["file:///tmp/figure%20%2520.png", "/tmp/figure %20.png"],
    ["C:/repo/figure.png", "C:/repo/figure.png"],
    ["file:///C:/repo/figure%20one.png", "C:/repo/figure one.png"],
  ])(
    "loads local image %s through the desktop asset protocol",
    (source, path) => {
      render(<Markdown value={`![figure](${source})`} workspacePath="/repo" />);

      expect(
        screen.getByRole("img", { name: "figure" }).getAttribute("src"),
      ).toBe(`asset://localhost/${encodeURIComponent(path)}`);
    },
  );

  it("preserves inline image data in the body", () => {
    render(<Markdown value={`![figure](${pixel})`} />);

    expect(
      screen.getByRole("img", { name: "figure" }).getAttribute("src"),
    ).toBe(pixel);
  });

  it("preserves HTTPS images in the body", () => {
    const source = "https://example.com/figure.png";
    render(<Markdown value={`![figure](${source})`} />);

    expect(
      screen.getByRole("img", { name: "figure" }).getAttribute("src"),
    ).toBe(source);
  });

  it("uses HTTPS for network images without a scheme in the desktop webview", () => {
    render(<Markdown value="![figure](//example.com/figure.png)" />);

    expect(
      screen.getByRole("img", { name: "figure" }).getAttribute("src"),
    ).toBe("https://example.com/figure.png");
  });

  it("keeps an existing desktop asset URL intact", () => {
    const source = "asset://localhost/%2Ftmp%2Ffigure.png";
    render(<Markdown value={`![figure](${source})`} />);

    expect(
      screen.getByRole("img", { name: "figure" }).getAttribute("src"),
    ).toBe(source);
  });

  it("resolves reference images against a Windows workspace and preserves the title", () => {
    render(
      <Markdown
        value={'![figure][plot]\n\n[plot]: ./images/figure.png "Result"'}
        workspacePath={"C:\\repo\\project"}
      />,
    );

    const image = screen.getByRole("img", { name: "figure" });
    expect(image.getAttribute("src")).toBe(
      "asset://localhost/C%3A%2Frepo%2Fproject%2Fimages%2Ffigure.png",
    );
    expect(image.getAttribute("title")).toBe("Result");
  });

  it.each([
    "javascript:alert(1)",
    "data:text/html;base64,PHNjcmlwdD4=",
    "mailto:test@example.com",
  ])("does not load a non-image URL: %s", (source) => {
    render(<Markdown value={`![figure](${source})`} />);

    expect(
      screen.getByRole("img", { name: "figure" }).getAttribute("src"),
    ).toBeNull();
  });

  it("does not allow data URLs in ordinary links", () => {
    render(<Markdown value={`[download](${pixel})`} />);

    expect(
      screen.getByText("download").closest("a")?.getAttribute("href"),
    ).toBeFalsy();
  });
});

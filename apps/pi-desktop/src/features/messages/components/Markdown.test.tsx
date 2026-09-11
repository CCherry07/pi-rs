// @vitest-environment jsdom
import { cleanup, createEvent, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { expectOpenedFileTarget } from "../test/fileLinkAssertions";
import { Markdown } from "./Markdown";

describe("Markdown file-like href behavior", () => {
  afterEach(() => {
    cleanup();
  });

  it("prevents file-like href navigation when no file opener is provided", () => {
    render(
      <Markdown
        value="See [setup](./docs/setup.md)"
        className="markdown"
      />,
    );

    const link = screen.getByText("setup").closest("a");
    expect(link?.getAttribute("href")).toBe("./docs/setup.md");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
  });

  it("intercepts file-like href clicks when a file opener is provided", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [setup](./docs/setup.md)"
        className="markdown"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("setup").closest("a");
    expect(link?.getAttribute("href")).toBe("./docs/setup.md");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "./docs/setup.md");
  });

  it("prevents bare relative link navigation without treating it as a file", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [setup](docs/setup.md)"
        className="markdown"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("setup").closest("a");
    expect(link?.getAttribute("href")).toBe("docs/setup.md");
    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expect(onOpenFileLink).not.toHaveBeenCalled();
  });

  it("still intercepts explicit workspace file hrefs when a file opener is provided", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [example](/workspace/src/example.ts)"
        className="markdown"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("example").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/src/example.ts");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "/workspace/src/example.ts");
  });

  it("still intercepts dotless workspace file hrefs when a file opener is provided", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [license](/workspace/sample-project/LICENSE)"
        className="markdown"
        workspacePath="/Users/example/projects/sample-project"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("license").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/sample-project/LICENSE");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "/workspace/sample-project/LICENSE");
  });

  it("intercepts mounted workspace links outside the old root allowlist", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [workflows](/workspace/.github/workflows)"
        className="markdown"
        workspacePath="/Users/example/projects/sample-project"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("workflows").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/.github/workflows");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "/workspace/.github/workflows");
  });

  it("intercepts mounted workspace directory links that resolve relative to the workspace", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [assets](/workspace/dist/assets)"
        className="markdown"
        workspacePath="/Users/example/projects/sample-project"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("assets").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/dist/assets");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "/workspace/dist/assets");
  });

  it("still intercepts nested workspace file hrefs when a file opener is provided", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [src](/workspaces/team/sample-project/src)"
        className="markdown"
        workspacePath="/Users/example/projects/sample-project"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("src").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspaces/team/sample-project/src");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "/workspaces/team/sample-project/src");
  });

  it("treats extensionless paths under /workspace/settings as files", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [license](/workspace/settings/LICENSE)"
        className="markdown"
        workspacePath="/Users/example/projects/settings"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("license").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/settings/LICENSE");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "/workspace/settings/LICENSE");
  });

  it("intercepts file hrefs that use #L line anchors", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [markdown](./docs/setup.md#L12)"
        className="markdown"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("markdown").closest("a");
    expect(link?.getAttribute("href")).toBe("./docs/setup.md#L12");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "./docs/setup.md", 12);
  });

  it("intercepts Windows absolute file hrefs with #L anchors and preserves the tooltip", () => {
    const onOpenFileLink = vi.fn();
    const onOpenFileLinkMenu = vi.fn();
    const linkedPath =
      "I:\\projects\\sample-project\\src\\features\\settings\\components\\sections\\SettingsDisplaySection.tsx#L422";
    render(
      <Markdown
        value={`See [SettingsDisplaySection.tsx](${linkedPath})`}
        className="markdown"
        onOpenFileLink={onOpenFileLink}
        onOpenFileLinkMenu={onOpenFileLinkMenu}
      />,
    );

    const link = screen.getByText("SettingsDisplaySection.tsx").closest("a");
    expect(link?.getAttribute("href")).toBe(
      "I:%5Cprojects%5Csample-project%5Csrc%5Cfeatures%5Csettings%5Ccomponents%5Csections%5CSettingsDisplaySection.tsx#L422",
    );
    expect(link?.getAttribute("title")).toBe(
      "I:\\projects\\sample-project\\src\\features\\settings\\components\\sections\\SettingsDisplaySection.tsx:422",
    );

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(
      onOpenFileLink,
      "I:\\projects\\sample-project\\src\\features\\settings\\components\\sections\\SettingsDisplaySection.tsx",
      422,
    );

    fireEvent.contextMenu(link as Element);
    expect(onOpenFileLinkMenu).toHaveBeenCalledWith(
      expect.anything(),
      {
        path: "I:\\projects\\sample-project\\src\\features\\settings\\components\\sections\\SettingsDisplaySection.tsx",
        line: 422,
        column: null,
      },
    );
  });

  it("prevents unsupported route fragments without treating them as file links", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [profile](/workspace/settings/profile#details)"
        className="markdown"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("profile").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/settings/profile#details");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expect(onOpenFileLink).not.toHaveBeenCalled();
  });

  it("keeps workspace settings #L anchors as local routes", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [settings](/workspace/settings#L12)"
        className="markdown"
        workspacePath="/Users/example/projects/sample-project"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("settings").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/settings#L12");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expect(onOpenFileLink).not.toHaveBeenCalled();
  });

  it("does not linkify workspace settings #L anchors in plain text", () => {
    const { container } = render(
      <Markdown
        value="See /workspace/settings#L12 for app settings."
        className="markdown"
        workspacePath="/Users/example/projects/sample-project"
      />,
    );

    expect(container.querySelector(".message-file-link")).toBeNull();
    expect(container.textContent).toContain("/workspace/settings#L12");
  });

  it("does not render slash commands as file paths", () => {
    const { container } = render(
      <Markdown
        value="/skill:performance-analysis 分析这周卡顿率"
        className="markdown"
        workspacePath="/Users/example/projects/workspace"
      />,
    );

    expect(container.querySelector(".message-file-link")).toBeNull();
    expect(container.textContent).toContain(
      "/skill:performance-analysis 分析这周卡顿率",
    );
    expect(container.textContent).not.toContain("../../../../");
  });

  it("does not linkify Windows file paths embedded in custom URIs", () => {
    const { container } = render(
      <Markdown
        value="Open vscode://file/C:/repo/src/App.tsx:12 in VS Code."
        className="markdown"
      />,
    );

    expect(container.querySelector(".message-file-link")).toBeNull();
    expect(container.textContent).toContain("vscode://file/C:/repo/src/App.tsx:12");
  });

  it("still opens mounted file links when the workspace basename is settings", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [app](/workspace/settings/src/App.tsx)"
        className="markdown"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("app").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/settings/src/App.tsx");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expectOpenedFileTarget(onOpenFileLink, "/workspace/settings/src/App.tsx");
  });

  it("keeps nested settings routes local when the workspace basename is settings", () => {
    const onOpenFileLink = vi.fn();
    render(
      <Markdown
        value="See [profile](/workspace/settings/profile)"
        className="markdown"
        workspacePath="/Users/example/projects/settings"
        onOpenFileLink={onOpenFileLink}
      />,
    );

    const link = screen.getByText("profile").closest("a");
    expect(link?.getAttribute("href")).toBe("/workspace/settings/profile");

    const clickEvent = createEvent.click(link as Element, {
      bubbles: true,
      cancelable: true,
    });
    fireEvent(link as Element, clickEvent);
    expect(clickEvent.defaultPrevented).toBe(true);
    expect(onOpenFileLink).not.toHaveBeenCalled();
  });

  it("wraps standard gfm tables in the styled table container", () => {
    const { container } = render(
      <Markdown
        value={["| Name | Value |", "| --- | --- |", "| Status | Ready |"].join("\n")}
        className="markdown"
      />,
    );

    expect(container.querySelector(".markdown-table-wrap")).toBeTruthy();
    expect(container.querySelector(".markdown-table")).toBeTruthy();
    expect(screen.getByRole("columnheader", { name: "Name" })).toBeTruthy();
    expect(screen.getByText("Ready")).toBeTruthy();
  });

  it("syntax-highlights fenced code with a recognized language", () => {
    const { container } = render(
      <Markdown
        value={["```typescript", "const answer: number = 42;", "console.log(answer);", "```"].join(
          "\n",
        )}
        className="markdown"
      />,
    );

    expect(container.querySelector("code.language-typescript .token.keyword")).toBeTruthy();
    expect(container.querySelector("code.language-typescript .token.number")).toBeTruthy();
  });

  it("syntax-highlights language aliases in message-style code blocks", () => {
    const { container } = render(
      <Markdown
        value={["```ts", "const answer = 42;", "console.log(answer);", "```"].join("\n")}
        className="markdown"
        codeBlockStyle="message"
      />,
    );

    expect(container.querySelector(".markdown-codeblock code.language-ts .token.keyword")).toBeTruthy();
    expect(container.querySelector(".markdown-codeblock code.language-ts .token.number")).toBeTruthy();
  });

  it("escapes fenced code when the language is unsupported", () => {
    const { container } = render(
      <Markdown
        value={["```unknown", "<script>window.pwned = true</script>", "plain text", "```"].join(
          "\n",
        )}
        className="markdown"
      />,
    );

    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("code.language-unknown")?.textContent).toContain(
      "<script>window.pwned = true</script>",
    );
  });

});

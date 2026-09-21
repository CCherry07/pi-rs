import { describe, expect, it } from "vitest";
import type { WorkspaceFileListing, WorkspaceRoot } from "../../types";
import { fileMentionText, workspaceFileMentions, workspaceFilePath } from "./workspaceFiles";

const app: WorkspaceRoot = { id: "app", name: "app", path: "/repos/app", ownership: { kind: "external" } };
const shared: WorkspaceRoot = { ...app, id: "shared", name: "shared", path: "/repos/shared files" };

describe("workspace file references", () => {
  it("keeps root identity separate from display names and quotes inserted paths", () => {
    const listing: WorkspaceFileListing = {
      workspace: { roots: [app, shared], primaryRoot: app.id, executionDir: app.path },
      files: [{ rootId: "app", path: "src/index.ts" }, { rootId: "shared", path: "src/index.ts" }], errors: [],
    };
    const mentions = workspaceFileMentions(listing);
    expect(mentions.map((file) => file.label)).toEqual(["app/src/index.ts", "shared/src/index.ts"]);
    expect(mentions[0].id).not.toBe(mentions[1].id);
    expect(mentions.map((file) => file.insertText)).toEqual(["/repos/app/src/index.ts", '"/repos/shared files/src/index.ts"']);
    const single = { roots: [app], primaryRoot: app.id, executionDir: app.path };
    expect(fileMentionText(single, app, "README.md")).toBe("README.md");
    expect(fileMentionText({ ...single, executionDir: app.path + "/src" }, app, "README.md")).toBe("/repos/app/README.md");
  });

  it("resolves Windows and filesystem roots without trimming file names", () => {
    expect(workspaceFilePath({ path: "C:\\repos\\app" }, "src/index.ts")).toBe("C:\\repos\\app\\src\\index.ts");
    expect(workspaceFilePath({ path: "/" }, " tmp ")).toBe("/ tmp ");
    expect(workspaceFilePath({ path: "/repos/app" }, "has\\backslash")).toBe("/repos/app/has\\backslash");
  });
});

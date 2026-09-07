import { describe, expect, it } from "vitest";
import { isKnownLocalWorkspaceRoutePath } from "./workspaceRoutePaths";

describe("isKnownLocalWorkspaceRoutePath", () => {
  it("matches the exact mounted settings route", () => {
    expect(isKnownLocalWorkspaceRoutePath("/workspace/settings")).toBe(true);
    expect(isKnownLocalWorkspaceRoutePath("/workspaces/team/settings")).toBe(true);
  });

  it("keeps explicit nested settings app routes out of file resolution", () => {
    expect(isKnownLocalWorkspaceRoutePath("/workspace/settings/profile")).toBe(true);
    expect(isKnownLocalWorkspaceRoutePath("/workspaces/team/settings/profile")).toBe(true);
  });

  it("still allows file-like descendants under reserved workspace names", () => {
    expect(isKnownLocalWorkspaceRoutePath("/workspace/settings/src/App.tsx")).toBe(false);
    expect(isKnownLocalWorkspaceRoutePath("/workspaces/team/settings/src/App.tsx")).toBe(
      false,
    );
  });

  it("treats extensionless descendants under reserved workspace names as mounted files", () => {
    expect(isKnownLocalWorkspaceRoutePath("/workspace/settings/LICENSE")).toBe(false);
    expect(isKnownLocalWorkspaceRoutePath("/workspaces/team/settings/Makefile")).toBe(
      false,
    );
  });
});

import { useCallback } from "react";
import type { GitHubPullRequest } from "../../../types";
import type { GitDiffSource, GitPanelMode } from "../types";
import { buildPullRequestDraft } from "../../../utils/pullRequestPrompt";

type UsePullRequestComposerOptions = {
  isCompact: boolean;
  setSelectedPullRequest: (pullRequest: GitHubPullRequest | null) => void;
  setDiffSource: (source: GitDiffSource) => void;
  setSelectedDiffPath: (path: string | null) => void;
  setCenterMode: (mode: "chat" | "diff") => void;
  setGitPanelMode: (mode: GitPanelMode) => void;
  setPrefillDraft: (draft: { id: string; text: string; createdAt: number }) => void;
  setActiveTab: (tab: "home" | "projects" | "chat" | "git" | "log") => void;
};

export function usePullRequestComposer({
  isCompact,
  setSelectedPullRequest,
  setDiffSource,
  setSelectedDiffPath,
  setCenterMode,
  setGitPanelMode,
  setPrefillDraft,
  setActiveTab,
}: UsePullRequestComposerOptions) {
  const handleSelectPullRequest = useCallback(
    (pullRequest: GitHubPullRequest) => {
      setSelectedPullRequest(pullRequest);
      setDiffSource("pr");
      setSelectedDiffPath(null);
      setCenterMode("diff");
      setGitPanelMode("prs");
      setPrefillDraft({
        id: `pr-prefill-${pullRequest.number}-${Date.now()}`,
        text: buildPullRequestDraft(pullRequest),
        createdAt: Date.now(),
      });
      if (isCompact) {
        setActiveTab("git");
      }
    },
    [
      isCompact,
      setActiveTab,
      setCenterMode,
      setDiffSource,
      setGitPanelMode,
      setPrefillDraft,
      setSelectedDiffPath,
      setSelectedPullRequest,
    ],
  );

  const resetPullRequestSelection = useCallback(() => {
    setDiffSource("local");
    setSelectedPullRequest(null);
  }, [setDiffSource, setSelectedPullRequest]);

  return {
    handleSelectPullRequest,
    resetPullRequestSelection,
  };
}

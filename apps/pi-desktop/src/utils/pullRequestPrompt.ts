import type { GitHubPullRequest } from "../types";

export function buildPullRequestDraft(pullRequest: GitHubPullRequest) {
  return `Question about PR #${pullRequest.number} (${pullRequest.title}):\n`;
}

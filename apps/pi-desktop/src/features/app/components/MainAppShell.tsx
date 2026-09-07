import { lazy, Suspense, type CSSProperties, type ComponentProps, type RefObject } from "react";
import { AppLayout } from "@app/components/AppLayout";
import { AppModals } from "@app/components/AppModals";
import type { AppModalsProps } from "@app/components/AppModals";
import {
  TitlebarExpandControls,
} from "@/features/layout/components/SidebarToggleControls";
import { WindowCaptionControls } from "@/features/layout/components/WindowCaptionControls";

const GitHubPanelData = lazy(() =>
  import("@/features/git/components/GitHubPanelData").then((module) => ({
    default: module.GitHubPanelData,
  })),
);

type MainAppShellProps = {
  appClassName: string;
  isResizing: boolean;
  appStyle: CSSProperties;
  appRef: RefObject<HTMLDivElement | null>;
  sidebarToggleProps: ComponentProps<typeof TitlebarExpandControls>;
  shouldLoadGitHubPanelData: boolean;
  gitHubPanelDataProps: {
    activeWorkspace: ComponentProps<typeof GitHubPanelData>["activeWorkspace"];
    gitPanelMode: ComponentProps<typeof GitHubPanelData>["gitPanelMode"];
    shouldLoadDiffs: ComponentProps<typeof GitHubPanelData>["shouldLoadDiffs"];
    diffSource: ComponentProps<typeof GitHubPanelData>["diffSource"];
    selectedPullRequestNumber: ComponentProps<typeof GitHubPanelData>["selectedPullRequestNumber"];
    onIssuesChange: ComponentProps<typeof GitHubPanelData>["onIssuesChange"];
    onPullRequestsChange: ComponentProps<typeof GitHubPanelData>["onPullRequestsChange"];
    onPullRequestDiffsChange: ComponentProps<typeof GitHubPanelData>["onPullRequestDiffsChange"];
    onPullRequestCommentsChange: ComponentProps<typeof GitHubPanelData>["onPullRequestCommentsChange"];
  };
  appLayoutProps: ComponentProps<typeof AppLayout>;
  appModalsProps: AppModalsProps;
};

export function MainAppShell({
  appClassName,
  isResizing,
  appStyle,
  appRef,
  sidebarToggleProps,
  shouldLoadGitHubPanelData,
  gitHubPanelDataProps,
  appLayoutProps,
  appModalsProps,
}: MainAppShellProps) {
  return (
    <div className={`${appClassName}${isResizing ? " is-resizing" : ""}`} style={appStyle} ref={appRef}>
      <div className="drag-strip" id="titlebar" />
      <TitlebarExpandControls {...sidebarToggleProps} />
      <WindowCaptionControls />
      {shouldLoadGitHubPanelData ? (
        <Suspense fallback={null}>
          <GitHubPanelData {...gitHubPanelDataProps} />
        </Suspense>
      ) : null}
      <AppLayout {...appLayoutProps} />
      <AppModals {...appModalsProps} />
    </div>
  );
}

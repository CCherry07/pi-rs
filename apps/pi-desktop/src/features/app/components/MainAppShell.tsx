import {
  lazy,
  Suspense,
  type CSSProperties,
  type ComponentProps,
  type RefObject,
} from "react";
import { AppLayout } from "@app/components/AppLayout";
import { AppModals } from "@app/components/AppModals";
import type { AppModalsProps } from "@app/components/AppModals";
import {
  TitlebarExpandControls,
  SidebarCollapseButton,
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
  gitHubPanelDataProps: ComponentProps<typeof GitHubPanelData>;
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
    <div
      className={`${appClassName}${isResizing ? " is-resizing" : ""}`}
      style={appStyle}
      ref={appRef}
    >
      <div className="drag-strip" id="titlebar" />
      <TitlebarExpandControls {...sidebarToggleProps} />
      <WindowCaptionControls />
      {shouldLoadGitHubPanelData ? (
        <Suspense fallback={null}>
          <GitHubPanelData {...gitHubPanelDataProps} />
        </Suspense>
      ) : null}
      <AppLayout
        {...appLayoutProps}
        desktopTopbarLeftNode={
          !appLayoutProps.isPhone && !appLayoutProps.isTablet ? (
            <div className="topbar-leading">
              <SidebarCollapseButton {...sidebarToggleProps} />
              {appLayoutProps.desktopTopbarLeftNode}
            </div>
          ) : (
            appLayoutProps.desktopTopbarLeftNode
          )
        }
      />
      <AppModals {...appModalsProps} />
    </div>
  );
}

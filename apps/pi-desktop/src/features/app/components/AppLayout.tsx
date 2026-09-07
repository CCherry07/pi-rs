import { memo } from "react";
import type { MouseEvent, ReactNode } from "react";
import { DesktopLayout } from "../../layout/components/DesktopLayout";
import { TabletLayout } from "../../layout/components/TabletLayout";
import { PhoneLayout } from "../../layout/components/PhoneLayout";
type AppLayoutProps = {
  isPhone: boolean;
  isTablet: boolean;
  showHome: boolean;
  showGitDetail: boolean;
  activeTab: "home" | "projects" | "chat" | "git" | "log";
  tabletTab: "chat" | "git" | "log";
  centerMode: "chat" | "diff";
  preloadGitDiffs: boolean;
  splitChatDiffView: boolean;
  hasActivePlan: boolean;
  activeWorkspace: boolean;
  sidebarNode: ReactNode;
  messagesNode: ReactNode;
  composerNode: ReactNode;
  updateToastNode: ReactNode;
  errorToastsNode: ReactNode;
  homeNode: ReactNode;
  mainHeaderNode: ReactNode;
  desktopTopbarLeftNode: ReactNode;
  topbarActionsNode?: ReactNode;
  tabletNavNode: ReactNode;
  tabBarNode: ReactNode;
  gitDiffPanelNode: ReactNode;
  gitDiffViewerNode: ReactNode;
  planPanelNode: ReactNode;
  debugPanelNode: ReactNode;
  debugPanelFullNode: ReactNode;
  terminalDockNode: ReactNode;
  compactEmptyChatNode: ReactNode;
  compactEmptyGitNode: ReactNode;
  compactGitBackNode: ReactNode;
  onSidebarResizeStart: (event: MouseEvent<HTMLDivElement>) => void;
  onChatDiffSplitPositionResizeStart: (event: MouseEvent<HTMLDivElement>) => void;
  onRightPanelResizeStart: (event: MouseEvent<HTMLDivElement>) => void;
  onPlanPanelResizeStart: (event: MouseEvent<HTMLDivElement>) => void;
};

export const AppLayout = memo(function AppLayout({
  isPhone,
  isTablet,
  showHome,
  showGitDetail,
  activeTab,
  tabletTab,
  centerMode,
  preloadGitDiffs,
  splitChatDiffView,
  hasActivePlan,
  activeWorkspace,
  sidebarNode,
  messagesNode,
  composerNode,
  updateToastNode,
  errorToastsNode,
  homeNode,
  mainHeaderNode,
  desktopTopbarLeftNode,
  topbarActionsNode,
  tabletNavNode,
  tabBarNode,
  gitDiffPanelNode,
  gitDiffViewerNode,
  planPanelNode,
  debugPanelNode,
  debugPanelFullNode,
  terminalDockNode,
  compactEmptyChatNode,
  compactEmptyGitNode,
  compactGitBackNode,
  onSidebarResizeStart,
  onChatDiffSplitPositionResizeStart,
  onRightPanelResizeStart,
  onPlanPanelResizeStart,
}: AppLayoutProps) {
  if (isPhone) {
    return (
      <PhoneLayout
        updateToastNode={updateToastNode}
        errorToastsNode={errorToastsNode}
        tabBarNode={tabBarNode}
        homeNode={homeNode}
        sidebarNode={sidebarNode}
        activeTab={activeTab}
        activeWorkspace={activeWorkspace}
        showGitDetail={showGitDetail}
        compactEmptyChatNode={compactEmptyChatNode}
        compactEmptyGitNode={compactEmptyGitNode}
        compactGitBackNode={compactGitBackNode}
        topbarLeftNode={mainHeaderNode}
        topbarActionsNode={topbarActionsNode}
        messagesNode={messagesNode}
        composerNode={composerNode}
        gitDiffPanelNode={gitDiffPanelNode}
        gitDiffViewerNode={gitDiffViewerNode}
        debugPanelNode={debugPanelFullNode}
      />
    );
  }

  if (isTablet) {
    return (
      <TabletLayout
        tabletNavNode={tabletNavNode}
        updateToastNode={updateToastNode}
        errorToastsNode={errorToastsNode}
        homeNode={homeNode}
        showHome={showHome}
        showWorkspace={activeWorkspace && !showHome}
        sidebarNode={sidebarNode}
        tabletTab={tabletTab}
        onSidebarResizeStart={onSidebarResizeStart}
        topbarLeftNode={mainHeaderNode}
        topbarActionsNode={topbarActionsNode}
        messagesNode={messagesNode}
        composerNode={composerNode}
        gitDiffPanelNode={gitDiffPanelNode}
        gitDiffViewerNode={gitDiffViewerNode}
        debugPanelNode={debugPanelFullNode}
      />
    );
  }

  return (
    <DesktopLayout
      sidebarNode={sidebarNode}
      updateToastNode={updateToastNode}
      errorToastsNode={errorToastsNode}
      homeNode={homeNode}
      showHome={showHome}
      showWorkspace={activeWorkspace && !showHome}
      topbarLeftNode={desktopTopbarLeftNode}
      topbarActionsNode={topbarActionsNode}
      centerMode={centerMode}
      preloadGitDiffs={preloadGitDiffs}
      splitChatDiffView={splitChatDiffView}
      messagesNode={messagesNode}
      gitDiffViewerNode={gitDiffViewerNode}
      gitDiffPanelNode={gitDiffPanelNode}
      planPanelNode={planPanelNode}
      composerNode={composerNode}
      terminalDockNode={terminalDockNode}
      debugPanelNode={debugPanelNode}
      hasActivePlan={hasActivePlan}
      onSidebarResizeStart={onSidebarResizeStart}
      onChatDiffSplitPositionResizeStart={onChatDiffSplitPositionResizeStart}
      onRightPanelResizeStart={onRightPanelResizeStart}
      onPlanPanelResizeStart={onPlanPanelResizeStart}
    />
  );
});

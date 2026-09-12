import { memo, useCallback, useContext, useId } from "react";
import { useTranslation } from "react-i18next";
import ChevronDown from "lucide-react/dist/esm/icons/chevron-down";
import ListTree from "lucide-react/dist/esm/icons/list-tree";
import type { ConversationItem, OpenAppTarget } from "../../../types";
import { PlanReadyFollowupMessage } from "../../app/components/PlanReadyFollowupMessage";
import { useFileLinkOpener } from "../hooks/useFileLinkOpener";
import { parseReasoning } from "../utils/messageRenderUtils";
import {
  DiffRow,
  ExploreRow,
  MessageRow,
  ReasoningRow,
  ToolRow,
  UserInputRow,
  WorkingIndicator,
} from "./MessageRows";
import { useMessagesViewState } from "./useMessagesViewState";
import { DesktopItemView, DesktopWorkspacePanels, useDesktopItemMatcher } from "../../extensions/ExtensionHost";
import type { DesktopViewHost } from "../../extensions/types";
import { RelatedSessionView } from "./RelatedSessionView";
import { Markdown } from "./Markdown";
import { useDesktopWidgets } from "../hooks/useDesktopWidgets";
import { runDesktopCommand } from "@services/tauri";
import { DesktopExtensionControls } from "../../app/components/DesktopExtensions";
import { ThreadConversationsContext } from "@threads/contexts/ThreadConversations";
import { ThreadContextBanner } from "./ThreadContextBanner";

type MessagesProps = {
  items: ConversationItem[];
  threadId: string | null;
  workspaceId?: string | null;
  isThinking: boolean;
  isLoadingMessages?: boolean;
  processingStartedAt?: number | null;
  lastDurationMs?: number | null;
  showPollingFetchStatus?: boolean;
  pollingIntervalMs?: number;
  workspacePath?: string | null;
  openTargets: OpenAppTarget[];
  selectedOpenAppId: string;
  codeBlockCopyUseModifier?: boolean;
  showMessageFilePath?: boolean;
  onPlanAccept?: () => void;
  onPlanSubmitChanges?: (changes: string) => void;
  onOpenThreadLink?: (threadId: string, workspaceId?: string | null) => void;
  onQuoteMessage?: (text: string) => void;
  onForkMessage?: (entryId: string) => void;
  embedded?: boolean;
  /** Frozen inherited history must never expand into live child conversations. */
  snapshot?: boolean;
  ancestorThreadIds?: readonly string[];
};

export const Messages = memo(function Messages({
  items,
  threadId,
  workspaceId = null,
  isThinking,
  isLoadingMessages = false,
  processingStartedAt = null,
  lastDurationMs = null,
  showPollingFetchStatus = false,
  pollingIntervalMs = 12000,
  workspacePath = null,
  openTargets,
  selectedOpenAppId,
  codeBlockCopyUseModifier = false,
  showMessageFilePath = true,
  onPlanAccept,
  onPlanSubmitChanges,
  onOpenThreadLink,
  onQuoteMessage,
  onForkMessage,
  embedded = false,
  snapshot = false,
  ancestorThreadIds = [],
}: MessagesProps) {
  const { t, i18n } = useTranslation("messages");
  const messageScopeId = useId();
  const conversations = useContext(ThreadConversationsContext);
  const inheritance = !snapshot && threadId
    ? conversations?.contextInheritanceByThread?.[threadId] : null;
  const contextDisclosureId = `inherited-context:${inheritance?.origin.snapshotEntryId ?? threadId}`;
  const { openFileLink, showFileLinkMenu } = useFileLinkOpener(
    workspacePath,
    openTargets,
    selectedOpenAppId,
  );
  const handleOpenThreadLink = useCallback(
    (threadId: string) => {
      onOpenThreadLink?.(threadId, workspaceId ?? null);
    },
    [onOpenThreadLink, workspaceId],
  );

  const widgetSnapshot = useDesktopWidgets(workspaceId, snapshot ? null : threadId);
  const hasDesktopView = useDesktopItemMatcher();
  const standaloneItem = useCallback((item: ConversationItem) => !snapshot && item.kind === "tool" && hasDesktopView(item), [snapshot, hasDesktopView]);
  const threadPath = threadId ? [...ancestorThreadIds, threadId] : ancestorThreadIds;
  const desktopHost: DesktopViewHost = {
    workspaceId, threadId, locale: i18n.language, readOnly: snapshot || embedded, frozen: snapshot,
    widgets: widgetSnapshot.widgets,
    runCommand: async (name,args) => {
      if(!workspaceId || !threadId || !widgetSnapshot.scopeToken) throw new Error("This session is not ready for commands");
      await runDesktopCommand(workspaceId,threadId,name,args,widgetSnapshot.scopeToken);
    },
    sessionStatus: reference => {
      const id=reference.sessionId;
      const status=id?conversations?.threadStatusById[id]:undefined;
      return status ? {isProcessing:status.isProcessing,totalTokens:id?conversations?.tokenUsageByThread[id]?.totalTokens??undefined:undefined}:undefined;
    },
    renderMarkdown: value => <Markdown value={value} workspacePath={workspacePath} onOpenFileLink={openFileLink} onOpenFileLinkMenu={showFileLinkMenu} onOpenThreadLink={handleOpenThreadLink} />,
    renderSession: reference => <RelatedSessionView reference={reference} workspaceId={workspaceId} threadId={threadId} ancestors={threadPath}
      renderConversation={conversation => <Messages {...conversation} embedded ancestorThreadIds={threadPath} workspaceId={workspaceId}
        workspacePath={workspacePath} openTargets={openTargets} selectedOpenAppId={selectedOpenAppId}
        codeBlockCopyUseModifier={codeBlockCopyUseModifier} showMessageFilePath={showMessageFilePath} onOpenThreadLink={onOpenThreadLink}/>} />,
  };

  const {
    bottomRef,
    containerRef,
    updateAutoScroll,
    requestAutoScroll,
    expandedItems,
    toggleExpanded,
    collapsedToolGroups,
    toggleToolGroup,
    copiedMessageId,
    handleCopyMessage,
    handleQuoteMessage,
    reasoningMetaById,
    latestReasoningLabel,
    groupedItems,
    planFollowup,
    dismissPlanFollowup,
  } = useMessagesViewState({
    standaloneItem,
    items,
    threadId,
    isThinking,
    activeUserInputRequestId: null,
    hasVisibleUserInputRequest: false,
    onPlanAccept,
    onPlanSubmitChanges,
    onQuoteMessage,
  });

  const planFollowupNode =
    planFollowup.shouldShow && onPlanAccept && onPlanSubmitChanges ? (
      <PlanReadyFollowupMessage
        onAccept={() => {
          dismissPlanFollowup();
          onPlanAccept();
        }}
        onSubmitChanges={(changes) => {
          dismissPlanFollowup();
          onPlanSubmitChanges(changes);
        }}
      />
    ) : null;

  const renderItem = (item: ConversationItem) => {
    if (item.kind === "message") {
      if (!item.text.trim() && !item.images?.length) {
        return null;
      }
      const isCopied = copiedMessageId === item.id;
      return (
        <MessageRow
          key={item.id}
          item={item}
          isCopied={isCopied}
          onCopy={handleCopyMessage}
          onQuote={onQuoteMessage ? handleQuoteMessage : undefined}
          onFork={
            onForkMessage && item.entryId
              ? (message) => {
                  if (message.entryId) {
                    onForkMessage(message.entryId);
                  }
                }
              : undefined
          }
          codeBlockCopyUseModifier={codeBlockCopyUseModifier}
          showMessageFilePath={showMessageFilePath}
          workspacePath={workspacePath}
          onOpenFileLink={openFileLink}
          onOpenFileLinkMenu={showFileLinkMenu}
          onOpenThreadLink={handleOpenThreadLink}
        />
      );
    }
    if (item.kind === "reasoning") {
      const isExpanded = expandedItems.has(item.id);
      const parsed = reasoningMetaById.get(item.id) ?? parseReasoning(item);
      return (
        <ReasoningRow
          key={item.id}
          item={item}
          parsed={parsed}
          isExpanded={isExpanded}
          onToggle={toggleExpanded}
          showMessageFilePath={showMessageFilePath}
          workspacePath={workspacePath}
          onOpenFileLink={openFileLink}
          onOpenFileLinkMenu={showFileLinkMenu}
          onOpenThreadLink={handleOpenThreadLink}
        />
      );
    }
    if (item.kind === "userInput") {
      const isExpanded = expandedItems.has(item.id);
      return (
        <UserInputRow
          key={item.id}
          item={item}
          isExpanded={isExpanded}
          onToggle={toggleExpanded}
        />
      );
    }
    if (item.kind === "diff") {
      return <DiffRow key={item.id} item={item} />;
    }
    if (item.kind === "tool") {
      const isExpanded = expandedItems.has(item.id);
      return (
        <DesktopItemView key={item.id} host={desktopHost} item={item} expanded={isExpanded} onToggle={() => toggleExpanded(item.id)} fallback={
        <ToolRow
          key={item.id}
          item={item}
          isExpanded={isExpanded}
          onToggle={toggleExpanded}
          showMessageFilePath={showMessageFilePath}
          workspacePath={workspacePath}
          onOpenFileLink={openFileLink}
          onOpenFileLinkMenu={showFileLinkMenu}
          onOpenThreadLink={handleOpenThreadLink}
          onRequestAutoScroll={requestAutoScroll}
        />} />
      );
    }
    if (item.kind === "explore") {
      return <ExploreRow key={item.id} item={item} />;
    }
    return null;
  };

  return (
    <div
      className={`messages ${embedded ? "messages-embedded" : "messages-full"}`}
      ref={containerRef}
      tabIndex={embedded ? 0 : undefined}
      onScroll={updateAutoScroll}
    >
      <div className="messages-inner">
        {!embedded && !snapshot && <><DesktopExtensionControls /><DesktopWorkspacePanels host={desktopHost} /></>}
        {inheritance && (
          <ThreadContextBanner
            inheritance={inheritance}
            isExpanded={expandedItems.has(contextDisclosureId)}
            onToggle={() => toggleExpanded(contextDisclosureId)}
            renderSnapshot={(inheritedItems) => (
              <Messages
                items={inheritedItems}
                threadId={threadId}
                snapshot
                embedded
                isThinking={false}
                workspaceId={workspaceId}
                workspacePath={workspacePath}
                openTargets={openTargets}
                selectedOpenAppId={selectedOpenAppId}
                codeBlockCopyUseModifier={codeBlockCopyUseModifier}
                showMessageFilePath={showMessageFilePath}
              />
            )}
          />
        )}
        {groupedItems.map((entry) => {
          if (entry.kind === "toolGroup") {
            const { group } = entry;
            const isCollapsed = collapsedToolGroups.has(group.id);
            const toolCalls = t("toolGroup.toolCalls", { count: group.toolCount });
            const summaryText =
              group.messageCount > 0
                ? t("toolGroup.summary", {
                    toolCalls,
                    messages: t("toolGroup.messages", { count: group.messageCount }),
                  })
                : toolCalls;
            const groupBodyId = `tool-group-${messageScopeId}-${group.id}`;
            return (
              <div
                key={`tool-group-${group.id}`}
                className={`tool-group ${isCollapsed ? "tool-group-collapsed" : ""}`}
              >
                <div className="tool-group-header">
                  <button
                    type="button"
                    className="tool-group-toggle"
                    onClick={() => toggleToolGroup(group.id)}
                    aria-expanded={!isCollapsed}
                    aria-controls={groupBodyId}
                    aria-label={
                      isCollapsed ? t("toolGroup.expand") : t("toolGroup.collapse")
                    }
                  >
                    <ListTree className="tool-group-icon" size={14} aria-hidden />
                    <span className="tool-group-summary">{summaryText}</span>
                    <span className="tool-group-chevron" aria-hidden>
                      <ChevronDown size={14} />
                    </span>
                  </button>
                </div>
                <div
                  className="tool-group-body"
                  id={groupBodyId}
                  aria-hidden={isCollapsed}
                  inert={isCollapsed ? true : undefined}
                >
                  <div className="tool-group-body-inner">
                    {group.items.map(renderItem)}
                  </div>
                </div>
              </div>
            );
          }
          return renderItem(entry.item);
        })}
        {planFollowupNode}
        <WorkingIndicator
          isThinking={isThinking}
          processingStartedAt={processingStartedAt}
          lastDurationMs={lastDurationMs}
          hasItems={items.length > 0}
          reasoningLabel={latestReasoningLabel}
          showPollingFetchStatus={showPollingFetchStatus}
          pollingIntervalMs={pollingIntervalMs}
        />
        {!items.length && !isThinking && !isLoadingMessages && (
          <div className="empty messages-empty">
            {snapshot ? t("relatedSession.snapshotEmpty") : embedded ? t("relatedSession.empty") : threadId ? t("empty.existingThread") : t("empty.newThread")}
          </div>
        )}
        {!items.length && !isThinking && isLoadingMessages && (
          <div className="empty messages-empty">
            <div className="messages-loading-indicator" role="status" aria-live="polite">
              <span className="working-spinner" aria-hidden />
              <span className="messages-loading-label">{t("empty.loading")}</span>
            </div>
          </div>
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  );
});

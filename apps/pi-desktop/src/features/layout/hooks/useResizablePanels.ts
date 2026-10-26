import type { MouseEvent as ReactMouseEvent } from "react";
import { useCallback, useEffect, useRef, useState } from "react";

const STORAGE_KEY_SIDEBAR = "pi-monitor.sidebarWidth";
const STORAGE_KEY_RIGHT_PANEL = "pi-monitor.rightPanelWidth";
const STORAGE_KEY_CHAT_DIFF_SPLIT_POSITION_PERCENT =
  "pi-monitor.chatDiffSplitPositionPercent";
const STORAGE_KEY_TERMINAL_PANEL = "pi-monitor.terminalPanelHeight";
const STORAGE_KEY_DEBUG_PANEL = "pi-monitor.debugPanelHeight";
const MIN_SIDEBAR_WIDTH = 220;
const MAX_SIDEBAR_WIDTH = 420;
const MIN_CHAT_DIFF_SPLIT_POSITION_PERCENT = 20;
const MAX_CHAT_DIFF_SPLIT_POSITION_PERCENT = 80;
const MIN_RIGHT_PANEL_WIDTH = 270;
const MAX_RIGHT_PANEL_WIDTH = 420;
const MIN_TERMINAL_PANEL_HEIGHT = 140;
const MAX_TERMINAL_PANEL_HEIGHT = 480;
const MIN_DEBUG_PANEL_HEIGHT = 120;
const MAX_DEBUG_PANEL_HEIGHT = 420;
const DEFAULT_SIDEBAR_WIDTH = 280;
const DEFAULT_CHAT_DIFF_SPLIT_POSITION_PERCENT = 50;
const DEFAULT_RIGHT_PANEL_WIDTH = 230;
const DEFAULT_TERMINAL_PANEL_HEIGHT = 220;
const DEFAULT_DEBUG_PANEL_HEIGHT = 180;

type ResizeState = {
  type:
    | "sidebar"
    | "right-panel"
    | "chat-diff-split"
    | "terminal-panel"
    | "debug-panel";
  startX: number;
  startY: number;
  startWidth: number;
  startHeight: number;
  startContainerWidth?: number;
  startContainerLeft?: number;
};

const CSS_VAR_MAP: Record<ResizeState["type"], { prop: string; unit: string }> =
  {
    sidebar: { prop: "--sidebar-width", unit: "px" },
    "right-panel": { prop: "--right-panel-width", unit: "px" },
    "chat-diff-split": {
      prop: "--chat-diff-split-position-percent",
      unit: "%",
    },
    "terminal-panel": { prop: "--terminal-panel-height", unit: "px" },
    "debug-panel": { prop: "--debug-panel-height", unit: "px" },
  };

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function readStoredWidth(
  key: string,
  fallback: number,
  min: number,
  max: number,
) {
  if (typeof window === "undefined") {
    return fallback;
  }
  const raw = window.localStorage.getItem(key);
  if (!raw) {
    return fallback;
  }
  const parsed = Number(raw);
  if (!Number.isFinite(parsed)) {
    return fallback;
  }
  return clamp(parsed, min, max);
}

function getContainerPointerPercent(event: MouseEvent, resize: ResizeState) {
  const containerWidth = resize.startContainerWidth ?? 1;
  const containerLeft = resize.startContainerLeft ?? 0;
  return ((event.clientX - containerLeft) / containerWidth) * 100;
}

export function useResizablePanels() {
  const [sidebarWidth, setSidebarWidth] = useState(() =>
    readStoredWidth(
      STORAGE_KEY_SIDEBAR,
      DEFAULT_SIDEBAR_WIDTH,
      MIN_SIDEBAR_WIDTH,
      MAX_SIDEBAR_WIDTH,
    ),
  );
  const [chatDiffSplitPositionPercent, setChatDiffSplitPositionPercent] =
    useState(() =>
      readStoredWidth(
        STORAGE_KEY_CHAT_DIFF_SPLIT_POSITION_PERCENT,
        DEFAULT_CHAT_DIFF_SPLIT_POSITION_PERCENT,
        MIN_CHAT_DIFF_SPLIT_POSITION_PERCENT,
        MAX_CHAT_DIFF_SPLIT_POSITION_PERCENT,
      ),
    );
  const [rightPanelWidth, setRightPanelWidth] = useState(() =>
    readStoredWidth(
      STORAGE_KEY_RIGHT_PANEL,
      DEFAULT_RIGHT_PANEL_WIDTH,
      MIN_RIGHT_PANEL_WIDTH,
      MAX_RIGHT_PANEL_WIDTH,
    ),
  );
  const [terminalPanelHeight, setTerminalPanelHeight] = useState(() =>
    readStoredWidth(
      STORAGE_KEY_TERMINAL_PANEL,
      DEFAULT_TERMINAL_PANEL_HEIGHT,
      MIN_TERMINAL_PANEL_HEIGHT,
      MAX_TERMINAL_PANEL_HEIGHT,
    ),
  );
  const [debugPanelHeight, setDebugPanelHeight] = useState(() =>
    readStoredWidth(
      STORAGE_KEY_DEBUG_PANEL,
      DEFAULT_DEBUG_PANEL_HEIGHT,
      MIN_DEBUG_PANEL_HEIGHT,
      MAX_DEBUG_PANEL_HEIGHT,
    ),
  );
  const resizeRef = useRef<ResizeState | null>(null);
  const appRef = useRef<HTMLDivElement | null>(null);
  const liveValueRef = useRef<number | null>(null);
  const [isResizing, setIsResizing] = useState(false);

  useEffect(() => {
    window.localStorage.setItem(STORAGE_KEY_SIDEBAR, String(sidebarWidth));
  }, [sidebarWidth]);

  useEffect(() => {
    window.localStorage.setItem(
      STORAGE_KEY_CHAT_DIFF_SPLIT_POSITION_PERCENT,
      String(chatDiffSplitPositionPercent),
    );
  }, [chatDiffSplitPositionPercent]);

  useEffect(() => {
    window.localStorage.setItem(
      STORAGE_KEY_RIGHT_PANEL,
      String(rightPanelWidth),
    );
  }, [rightPanelWidth]);

  useEffect(() => {
    window.localStorage.setItem(
      STORAGE_KEY_TERMINAL_PANEL,
      String(terminalPanelHeight),
    );
  }, [terminalPanelHeight]);

  useEffect(() => {
    window.localStorage.setItem(
      STORAGE_KEY_DEBUG_PANEL,
      String(debugPanelHeight),
    );
  }, [debugPanelHeight]);

  useEffect(() => {
    function handleMouseMove(event: MouseEvent) {
      const resize = resizeRef.current;
      const el = appRef.current;
      if (!resize || !el) {
        return;
      }
      event.preventDefault();

      let next: number;
      if (resize.type === "sidebar") {
        const delta = event.clientX - resize.startX;
        next = clamp(
          resize.startWidth + delta,
          MIN_SIDEBAR_WIDTH,
          MAX_SIDEBAR_WIDTH,
        );
      } else if (resize.type === "chat-diff-split") {
        const pointerPercent = getContainerPointerPercent(event, resize);
        next = clamp(
          pointerPercent,
          MIN_CHAT_DIFF_SPLIT_POSITION_PERCENT,
          MAX_CHAT_DIFF_SPLIT_POSITION_PERCENT,
        );
      } else if (resize.type === "right-panel") {
        const delta = event.clientX - resize.startX;
        next = clamp(
          resize.startWidth - delta,
          MIN_RIGHT_PANEL_WIDTH,
          MAX_RIGHT_PANEL_WIDTH,
        );
      } else if (resize.type === "terminal-panel") {
        const delta = event.clientY - resize.startY;
        next = clamp(
          resize.startHeight - delta,
          MIN_TERMINAL_PANEL_HEIGHT,
          MAX_TERMINAL_PANEL_HEIGHT,
        );
      } else {
        const delta = event.clientY - resize.startY;
        next = clamp(
          resize.startHeight - delta,
          MIN_DEBUG_PANEL_HEIGHT,
          MAX_DEBUG_PANEL_HEIGHT,
        );
      }

      liveValueRef.current = next;
      const { prop, unit } = CSS_VAR_MAP[resize.type];
      el.style.setProperty(prop, `${next}${unit}`);
    }

    function handleMouseUp() {
      const resize = resizeRef.current;
      if (!resize) {
        return;
      }
      const finalValue = liveValueRef.current;
      if (finalValue !== null) {
        switch (resize.type) {
          case "sidebar":
            setSidebarWidth(finalValue);
            break;
          case "chat-diff-split":
            setChatDiffSplitPositionPercent(finalValue);
            break;
          case "right-panel":
            setRightPanelWidth(finalValue);
            break;
          case "terminal-panel":
            setTerminalPanelHeight(finalValue);
            break;
          case "debug-panel":
            setDebugPanelHeight(finalValue);
            break;
        }
      }
      resizeRef.current = null;
      liveValueRef.current = null;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      setIsResizing(false);
    }

    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
    return () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };
  }, []);

  const onSidebarResizeStart = useCallback(
    (event: ReactMouseEvent) => {
      event.preventDefault();
      resizeRef.current = {
        type: "sidebar",
        startX: event.clientX,
        startY: event.clientY,
        startWidth: sidebarWidth,
        startHeight: 0,
      };
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      setIsResizing(true);
    },
    [sidebarWidth],
  );

  const onChatDiffSplitPositionResizeStart = useCallback(
    (event: ReactMouseEvent) => {
      event.preventDefault();

      const content = event.currentTarget.closest(
        ".content-split",
      ) as HTMLDivElement | null;
      resizeRef.current = {
        type: "chat-diff-split",
        startX: event.clientX,
        startY: event.clientY,
        startWidth: chatDiffSplitPositionPercent,
        startHeight: 0,
        startContainerWidth: content?.clientWidth,
        startContainerLeft: content?.getBoundingClientRect().left,
      };
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      setIsResizing(true);
    },
    [chatDiffSplitPositionPercent],
  );

  const onRightPanelResizeStart = useCallback(
    (event: ReactMouseEvent) => {
      event.preventDefault();

      resizeRef.current = {
        type: "right-panel",
        startX: event.clientX,
        startY: event.clientY,
        startWidth: rightPanelWidth,
        startHeight: 0,
      };
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      setIsResizing(true);
    },
    [rightPanelWidth],
  );

  const onTerminalPanelResizeStart = useCallback(
    (event: ReactMouseEvent) => {
      event.preventDefault();

      resizeRef.current = {
        type: "terminal-panel",
        startX: event.clientX,
        startY: event.clientY,
        startWidth: rightPanelWidth,
        startHeight: terminalPanelHeight,
      };
      document.body.style.cursor = "row-resize";
      document.body.style.userSelect = "none";
      setIsResizing(true);
    },
    [rightPanelWidth, terminalPanelHeight],
  );

  const onDebugPanelResizeStart = useCallback(
    (event: ReactMouseEvent) => {
      event.preventDefault();

      resizeRef.current = {
        type: "debug-panel",
        startX: event.clientX,
        startY: event.clientY,
        startWidth: rightPanelWidth,
        startHeight: debugPanelHeight,
      };
      document.body.style.cursor = "row-resize";
      document.body.style.userSelect = "none";
      setIsResizing(true);
    },
    [debugPanelHeight, rightPanelWidth],
  );

  return {
    appRef,
    isResizing,
    sidebarWidth,
    rightPanelWidth,
    terminalPanelHeight,
    debugPanelHeight,
    onSidebarResizeStart,
    chatDiffSplitPositionPercent,
    onChatDiffSplitPositionResizeStart,
    onRightPanelResizeStart,
    onTerminalPanelResizeStart,
    onDebugPanelResizeStart,
  };
}

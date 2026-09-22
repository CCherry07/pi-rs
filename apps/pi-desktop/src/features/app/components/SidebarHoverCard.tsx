import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { PopoverSurface } from "../../design-system/components/popover/PopoverPrimitives";

const OPEN_DELAY = 300;
const CLOSE_DELAY = 180;
const OPEN_EVENT = "pi:sidebar-hover-open";

type Controls = {
  isOpen: boolean;
  panelId: string;
  open: (focus?: boolean) => void;
  close: () => void;
  toggle: (focus?: boolean) => void;
};

type SidebarHoverCardProps = {
  label: string;
  content: ReactNode | ((close: () => void) => ReactNode);
  disabled?: boolean;
  children: (controls: Controls) => ReactNode;
};

export function SidebarHoverCard({ label, content, disabled = false, children }: SidebarHoverCardProps) {
  const panelId = useId();
  const [isOpen, setIsOpen] = useState(false);
  const anchorRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pinnedRef = useRef(false);
  const focusPanelRef = useRef(false);

  const cancelTimer = useCallback(() => {
    if (timerRef.current !== null) clearTimeout(timerRef.current);
    timerRef.current = null;
  }, []);

  const close = useCallback(() => {
    cancelTimer();
    pinnedRef.current = false;
    focusPanelRef.current = false;
    setIsOpen(false);
  }, [cancelTimer]);

  const focusPanel = useCallback(() => {
    const panel = panelRef.current;
    (panel?.querySelector<HTMLElement>("button:not(:disabled), [tabindex='0']") ?? panel)?.focus();
  }, []);

  const open = useCallback((focus = false) => {
    if (disabled) return;
    cancelTimer();
    pinnedRef.current = focus;
    focusPanelRef.current = focus;
    window.dispatchEvent(new CustomEvent(OPEN_EVENT, { detail: panelId }));
    setIsOpen(true);
    if (focus) focusPanel();
  }, [cancelTimer, disabled, focusPanel, panelId]);

  const scheduleOpen = () => {
    cancelTimer();
    if (!disabled && !isOpen) timerRef.current = setTimeout(() => open(), OPEN_DELAY);
  };

  const scheduleClose = () => {
    cancelTimer();
    if (pinnedRef.current) return;
    timerRef.current = setTimeout(() => {
      if (!panelRef.current?.contains(document.activeElement)) close();
    }, CLOSE_DELAY);
  };

  useEffect(() => cancelTimer, [cancelTimer]);

  useEffect(() => { if (disabled) close(); }, [close, disabled]);

  useEffect(() => {
    const handleOtherOpen = (event: Event) => {
      if ((event as CustomEvent<string>).detail !== panelId) close();
    };
    window.addEventListener(OPEN_EVENT, handleOtherOpen);
    return () => window.removeEventListener(OPEN_EVENT, handleOtherOpen);
  }, [close, panelId]);

  useLayoutEffect(() => {
    if (!isOpen || disabled) return;
    const anchor = anchorRef.current;
    const panel = panelRef.current;
    if (!anchor || !panel) return;
    const placePanel = () => {
      const bounds = anchor.getBoundingClientRect();
      const width = panel.offsetWidth;
      const height = panel.offsetHeight;
      const maxLeft = Math.max(12, window.innerWidth - width - 12);
      const preferredLeft = bounds.right + 8 <= maxLeft
        ? bounds.right + 8
        : bounds.left - width - 8 >= 12 ? bounds.left - width - 8 : bounds.left;
      panel.style.left = `${Math.max(12, Math.min(preferredLeft, maxLeft))}px`;
      panel.style.top = `${Math.max(12, Math.min(bounds.top, window.innerHeight - height - 12))}px`;
    };
    placePanel();
    if (focusPanelRef.current) focusPanel();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(placePanel);
    observer?.observe(panel);
    window.addEventListener("resize", placePanel);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", placePanel);
    };
  }, [disabled, focusPanel, isOpen]);

  useEffect(() => {
    if (!isOpen) return;
    const inside = (target: EventTarget | null) => target instanceof Node &&
      (anchorRef.current?.contains(target) || panelRef.current?.contains(target));
    const handlePointerDown = (event: PointerEvent) => { if (!inside(event.target)) close(); };
    const handleFocus = (event: FocusEvent) => { if (!inside(event.target)) close(); };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      const returnFocus = panelRef.current?.contains(document.activeElement);
      close();
      if (returnFocus) {
        (anchorRef.current?.querySelector<HTMLElement>(".sidebar-details-trigger") ??
          anchorRef.current?.querySelector<HTMLElement>("[tabindex='0']"))?.focus();
      }
    };
    const handleScroll = (event: Event) => {
      if (!(event.target instanceof Node && panelRef.current?.contains(event.target))) close();
    };
    window.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("focusin", handleFocus);
    window.addEventListener("keydown", handleEscape);
    window.addEventListener("scroll", handleScroll, true);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("focusin", handleFocus);
      window.removeEventListener("keydown", handleEscape);
      window.removeEventListener("scroll", handleScroll, true);
    };
  }, [close, isOpen, panelId]);

  return <div className="sidebar-hover-anchor" ref={anchorRef}
    onMouseEnter={scheduleOpen} onMouseLeave={scheduleClose}
    onKeyDown={(event) => {
      if (event.key === "ArrowRight" && !disabled) {
        event.preventDefault();
        event.stopPropagation();
        open(true);
      }
    }}>
    {children({ isOpen: isOpen && !disabled, panelId, open, close, toggle: (focus = true) => {
      if (isOpen && pinnedRef.current) close();
      else open(focus);
    } })}
    {isOpen && !disabled && createPortal(
      <PopoverSurface id={panelId} ref={panelRef} className="sidebar-hovercard"
        role="dialog" aria-label={label} tabIndex={-1}
        onMouseEnter={cancelTimer} onMouseLeave={scheduleClose}
        onClick={(event) => event.stopPropagation()}
        onContextMenu={(event) => event.stopPropagation()}
        onKeyDown={(event) => { if (event.key !== "Escape") event.stopPropagation(); }}>
        <div className="sidebar-hovercard-title">{label}</div>
        {typeof content === "function" ? content(close) : content}
      </PopoverSurface>, document.body,
    )}
  </div>;
}

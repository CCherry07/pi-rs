import { useEffect, useState, type RefObject } from "react";

type UseComposerInputLayoutArgs = {
  isExpanded: boolean;
  text: string;
  textareaRef: RefObject<HTMLTextAreaElement | null>;
};

export function useComposerInputLayout({
  isExpanded,
  text,
  textareaRef,
}: UseComposerInputLayoutArgs) {
  const [isPhoneLayout, setIsPhoneLayout] = useState(false);
  const [isPhoneTallInput, setIsPhoneTallInput] = useState(false);

  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) {
      return;
    }
    const appRoot = textarea.closest(".app");
    if (!(appRoot instanceof HTMLElement)) {
      setIsPhoneLayout(false);
      return;
    }

    const syncLayout = () => {
      const nextIsPhoneLayout = appRoot.classList.contains("layout-phone");
      setIsPhoneLayout((prev) => (prev === nextIsPhoneLayout ? prev : nextIsPhoneLayout));
    };

    syncLayout();
    const observer = new MutationObserver((records) => {
      if (records.some((record) => record.attributeName === "class")) {
        syncLayout();
      }
    });
    observer.observe(appRoot, { attributes: true, attributeFilter: ["class"] });
    return () => {
      observer.disconnect();
    };
  }, [textareaRef]);

  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) {
      return;
    }
    const minTextareaHeight = isExpanded ? (isPhoneLayout ? 152 : 180) : isPhoneLayout ? 52 : 60;
    const maxTextareaHeight = isExpanded ? (isPhoneLayout ? 280 : 320) : isPhoneLayout ? 168 : 120;
    textarea.style.height = "auto";
    textarea.style.minHeight = `${minTextareaHeight}px`;
    textarea.style.maxHeight = `${maxTextareaHeight}px`;
    const nextHeight = Math.min(
      Math.max(textarea.scrollHeight, minTextareaHeight),
      maxTextareaHeight,
    );
    textarea.style.height = `${nextHeight}px`;
    textarea.style.overflowY = textarea.scrollHeight > maxTextareaHeight ? "auto" : "hidden";

    if (!isPhoneLayout) {
      setIsPhoneTallInput((prev) => (prev ? false : prev));
      return;
    }

    const computedStyle = window.getComputedStyle(textarea);
    const lineHeight = Number.parseFloat(computedStyle.lineHeight) || 20;
    const paddingTop = Number.parseFloat(computedStyle.paddingTop) || 0;
    const paddingBottom = Number.parseFloat(computedStyle.paddingBottom) || 0;
    const contentHeight = Math.max(0, nextHeight - paddingTop - paddingBottom);
    const estimatedLineCount = contentHeight / lineHeight;
    const nextIsPhoneTallInput = estimatedLineCount > 2.25;
    setIsPhoneTallInput((prev) => (prev === nextIsPhoneTallInput ? prev : nextIsPhoneTallInput));
  }, [isExpanded, isPhoneLayout, text, textareaRef]);

  return { isPhoneLayout, isPhoneTallInput };
}

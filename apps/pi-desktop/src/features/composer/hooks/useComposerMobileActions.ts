import { useEffect, useRef, useState } from "react";

type UseComposerMobileActionsArgs = {
  disabled: boolean;
};

export function useComposerMobileActions({ disabled }: UseComposerMobileActionsArgs) {
  const mobileActionsRef = useRef<HTMLDivElement | null>(null);
  const [mobileActionsOpen, setMobileActionsOpen] = useState(false);

  useEffect(() => {
    if (!mobileActionsOpen) {
      return;
    }

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (target && mobileActionsRef.current?.contains(target)) {
        return;
      }
      setMobileActionsOpen(false);
    };

    const handleKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") {
        setMobileActionsOpen(false);
      }
    };

    window.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [mobileActionsOpen]);

  useEffect(() => {
    if (disabled && mobileActionsOpen) {
      setMobileActionsOpen(false);
    }
  }, [disabled, mobileActionsOpen]);

  return {
    mobileActionsOpen,
    mobileActionsRef,
    setMobileActionsOpen,
  };
}

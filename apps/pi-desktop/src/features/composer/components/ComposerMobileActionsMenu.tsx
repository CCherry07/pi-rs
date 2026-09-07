import type { Dispatch, RefObject, SetStateAction } from "react";
import { useTranslation } from "react-i18next";
import ChevronDown from "lucide-react/dist/esm/icons/chevron-down";
import ChevronUp from "lucide-react/dist/esm/icons/chevron-up";
import ImagePlus from "lucide-react/dist/esm/icons/image-plus";
import Mic from "lucide-react/dist/esm/icons/mic";
import Plus from "lucide-react/dist/esm/icons/plus";
import Square from "lucide-react/dist/esm/icons/square";
import X from "lucide-react/dist/esm/icons/x";
import {
  PopoverMenuItem,
  PopoverSurface,
} from "../../design-system/components/popover/PopoverPrimitives";

type ComposerMobileActionsMenuProps = {
  disabled: boolean;
  handleMobileAttachClick: () => void;
  handleMobileDictationClick: () => void;
  handleMobileExpandClick: () => void;
  isDictating: boolean;
  isDictationProcessing: boolean;
  isExpanded: boolean;
  micAriaLabel: string;
  micDisabled: boolean;
  mobileActionsOpen: boolean;
  mobileActionsRef: RefObject<HTMLDivElement | null>;
  onAddAttachment?: () => void;
  onToggleExpand?: () => void;
  setMobileActionsOpen: Dispatch<SetStateAction<boolean>>;
  showDictationAction: boolean;
};

export function ComposerMobileActionsMenu({
  disabled,
  handleMobileAttachClick,
  handleMobileDictationClick,
  handleMobileExpandClick,
  isDictating,
  isDictationProcessing,
  isExpanded,
  micAriaLabel,
  micDisabled,
  mobileActionsOpen,
  mobileActionsRef,
  onAddAttachment,
  onToggleExpand,
  setMobileActionsOpen,
  showDictationAction,
}: ComposerMobileActionsMenuProps) {
  const { t } = useTranslation("messages");
  return (
    <div
      className={`composer-mobile-menu${mobileActionsOpen ? " is-open" : ""}`}
      ref={mobileActionsRef}
    >
      <button
        type="button"
        className="composer-action composer-action--mobile-menu"
        onClick={() => setMobileActionsOpen((prev) => !prev)}
        disabled={disabled}
        aria-expanded={mobileActionsOpen}
        aria-haspopup="menu"
        aria-label={t("composer.mobile.more")}
        title={t("composer.mobile.more")}
      >
        <Plus size={14} aria-hidden />
      </button>
      {mobileActionsOpen && (
        <PopoverSurface className="composer-mobile-actions-popover" role="menu">
          <PopoverMenuItem
            onClick={handleMobileAttachClick}
            disabled={disabled || !onAddAttachment}
            icon={<ImagePlus size={14} />}
          >
            {t("composer.addImage")}
          </PopoverMenuItem>
          {onToggleExpand && (
            <PopoverMenuItem
              onClick={handleMobileExpandClick}
              disabled={disabled}
              icon={
                isExpanded ? <ChevronDown size={14} /> : <ChevronUp size={14} />
              }
            >
              {isExpanded ? t("composer.collapseInput") : t("composer.expandInput")}
            </PopoverMenuItem>
          )}
          {showDictationAction && (
            <PopoverMenuItem
              onClick={handleMobileDictationClick}
              disabled={micDisabled}
              icon={
                isDictationProcessing ? (
                  <X size={14} />
                ) : isDictating ? (
                  <Square size={14} />
                ) : (
                  <Mic size={14} />
                )
              }
            >
              {micAriaLabel}
            </PopoverMenuItem>
          )}
        </PopoverSurface>
      )}
    </div>
  );
}

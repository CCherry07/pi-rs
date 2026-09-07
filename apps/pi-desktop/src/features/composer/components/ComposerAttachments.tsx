import { convertFileSrc } from "@tauri-apps/api/core";
import Image from "lucide-react/dist/esm/icons/image";
import X from "lucide-react/dist/esm/icons/x";
import { useTranslation } from "react-i18next";

type ComposerAttachmentsProps = {
  attachments: string[];
  disabled: boolean;
  onRemoveAttachment?: (path: string) => void;
};

function fileTitle(path: string, pastedImageLabel: string, imageLabel: string) {
  if (path.startsWith("data:")) {
    return pastedImageLabel;
  }
  if (path.startsWith("http://") || path.startsWith("https://")) {
    return imageLabel;
  }
  const normalized = path.replace(/\\/g, "/");
  const parts = normalized.split("/").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : path;
}

function attachmentPreviewSrc(path: string) {
  if (path.startsWith("data:")) {
    return path;
  }
  if (path.startsWith("http://") || path.startsWith("https://")) {
    return path;
  }
  try {
    return convertFileSrc(path);
  } catch {
    return "";
  }
}

export function ComposerAttachments({
  attachments,
  disabled,
  onRemoveAttachment,
}: ComposerAttachmentsProps) {
  const { t } = useTranslation("messages");

  if (attachments.length === 0) {
    return null;
  }

  return (
    <div className="composer-attachments">
      {attachments.map((path) => {
        const title = fileTitle(
          path,
          t("composer.pastedImage"),
          t("composer.image"),
        );
        const titleAttr = path.startsWith("data:")
          ? t("composer.pastedImage")
          : path;
        const previewSrc = attachmentPreviewSrc(path);
        return (
          <div
            key={path}
            className="composer-attachment"
            title={titleAttr}
          >
            {previewSrc && (
              <span className="composer-attachment-preview" aria-hidden>
                <img src={previewSrc} alt="" />
              </span>
            )}
            {previewSrc ? (
              <span className="composer-attachment-thumb" aria-hidden>
                <img src={previewSrc} alt="" />
              </span>
            ) : (
              <span className="composer-icon" aria-hidden>
                <Image size={14} />
              </span>
            )}
            <span className="composer-attachment-name">{title}</span>
            <button
              type="button"
              className="composer-attachment-remove"
              onClick={() => onRemoveAttachment?.(path)}
              aria-label={t("composer.removeAttachment", { name: title })}
              disabled={disabled}
            >
              <X size={12} aria-hidden />
            </button>
          </div>
        );
      })}
    </div>
  );
}

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { i18n } from "../src/i18n";
import { AppearancePreview } from "../src/design-preview/AppearancePreview";

// Tauri event registration is inert here. No native project or provider is used.
mockWindows("main");
mockIPC((command) => {
  if (command === "plugin:event|listen") return 0;
  if (command === "plugin:event|unlisten") return;
  throw new Error(`Native command is unavailable in the appearance preview: ${command}`);
});
void i18n.changeLanguage("zh-CN");
const root = createRoot(document.getElementById("root")!);
root.render(<StrictMode><AppearancePreview /></StrictMode>);
import.meta.hot?.dispose(() => { root.unmount(); clearMocks(); });

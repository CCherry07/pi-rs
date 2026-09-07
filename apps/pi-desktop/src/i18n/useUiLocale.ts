import { useEffect } from "react";
import type { UiLocale } from "@/types";
import { setNativeUiLocale } from "@/services/tauri";
import { i18n } from "./index";
import { resolveUiLocale } from "./locale";

export function useUiLocale(preference: UiLocale) {
  useEffect(() => {
    const applyLocale = () => {
      const locale = resolveUiLocale(preference);
      void i18n.changeLanguage(locale);
      void setNativeUiLocale(locale).catch((error) => {
        console.warn("Failed to update native UI locale", error);
      });
      if (typeof document !== "undefined") {
        document.documentElement.lang = locale;
        document.documentElement.dir = i18n.dir(locale);
      }
    };

    applyLocale();
    if (preference !== "system" || typeof window === "undefined") {
      return undefined;
    }

    window.addEventListener("languagechange", applyLocale);
    return () => {
      window.removeEventListener("languagechange", applyLocale);
    };
  }, [preference]);
}

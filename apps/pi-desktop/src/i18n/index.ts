import i18next from "i18next";
import { initReactI18next } from "react-i18next";
import { DEFAULT_UI_LOCALE, matchSupportedUiLocale } from "./locale";
import { defaultNS, resources } from "./resources";

export const i18n = i18next.createInstance();

void i18n.use(initReactI18next).init({
  resources,
  lng: matchSupportedUiLocale(
    typeof navigator === "undefined"
      ? []
      : navigator.languages.length > 0
        ? navigator.languages
        : [navigator.language],
  ),
  fallbackLng: DEFAULT_UI_LOCALE,
  supportedLngs: ["en", "zh-CN"],
  defaultNS,
  fallbackNS: "common",
  returnNull: false,
  initAsync: false,
  interpolation: {
    escapeValue: false,
  },
  react: {
    useSuspense: false,
  },
});

export default i18n;

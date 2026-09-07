import enCommon from "./locales/en/common.json";
import enApp from "./locales/en/app.json";
import enMessages from "./locales/en/messages.json";
import enSettings from "./locales/en/settings.json";
import enGit from "./locales/en/git.json";
import enWorkspaces from "./locales/en/workspaces.json";
import enHome from "./locales/en/home.json";
import enPrompts from "./locales/en/prompts.json";
import zhCnCommon from "./locales/zh-CN/common.json";
import zhCnApp from "./locales/zh-CN/app.json";
import zhCnMessages from "./locales/zh-CN/messages.json";
import zhCnSettings from "./locales/zh-CN/settings.json";
import zhCnGit from "./locales/zh-CN/git.json";
import zhCnWorkspaces from "./locales/zh-CN/workspaces.json";
import zhCnHome from "./locales/zh-CN/home.json";
import zhCnPrompts from "./locales/zh-CN/prompts.json";

export const defaultNS = "common" as const;

export const resources = {
  en: {
    app: enApp,
    common: enCommon,
    messages: enMessages,
    settings: enSettings,
    git: enGit,
    workspaces: enWorkspaces,
    home: enHome,
    prompts: enPrompts,
  },
  "zh-CN": {
    app: zhCnApp,
    common: zhCnCommon,
    messages: zhCnMessages,
    settings: zhCnSettings,
    git: zhCnGit,
    workspaces: zhCnWorkspaces,
    home: zhCnHome,
    prompts: zhCnPrompts,
  },
} as const;

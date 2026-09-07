// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { useTranslation } from "react-i18next";
import type { UiLocale } from "@/types";
import { i18n } from "./index";
import { useUiLocale } from "./useUiLocale";

function LocaleProbe({ preference }: { preference: UiLocale }) {
  useUiLocale(preference);
  const { t } = useTranslation("settings");
  return <div>{t("display.title")}</div>;
}

describe("useUiLocale", () => {
  afterEach(async () => {
    cleanup();
    await i18n.changeLanguage("en");
    document.documentElement.lang = "";
    document.documentElement.dir = "";
  });

  it("switches bundled translations without reloading", async () => {
    const view = render(<LocaleProbe preference="en" />);
    expect(screen.getByText("Display & Sound")).toBeTruthy();

    view.rerender(<LocaleProbe preference="zh-CN" />);

    await waitFor(() => expect(screen.getByText("显示与声音")).toBeTruthy());
    expect(document.documentElement.lang).toBe("zh-CN");
    expect(document.documentElement.dir).toBe("ltr");
  });
});

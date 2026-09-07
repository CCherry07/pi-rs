// @vitest-environment jsdom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { PanelTabs, type PanelTabId } from "./PanelTabs";

function PanelTabsHarness() {
  const [active, setActive] = useState<PanelTabId>("git");
  return <PanelTabs active={active} onSelect={setActive} />;
}

describe("PanelTabs", () => {
  it("moves selection and focus with arrow keys", async () => {
    render(<PanelTabsHarness />);
    const tabs = screen.getAllByRole("tab");

    tabs[0].focus();
    fireEvent.keyDown(tabs[0], { key: "ArrowRight" });

    await waitFor(() => {
      expect(tabs[1].getAttribute("aria-selected")).toBe("true");
      expect(document.activeElement).toBe(tabs[1]);
    });

    fireEvent.keyDown(tabs[1], { key: "ArrowRight" });

    await waitFor(() => {
      expect(tabs[2].getAttribute("aria-selected")).toBe("true");
      expect(document.activeElement).toBe(tabs[2]);
    });
  });
});

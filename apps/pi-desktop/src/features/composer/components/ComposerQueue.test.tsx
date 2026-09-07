/** @vitest-environment jsdom */
import { fireEvent, render, screen } from "@testing-library/react";
import { cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { QueuedMessage } from "../../../types";
import { ComposerQueue } from "./ComposerQueue";

const queuedItem: QueuedMessage = {
  id: "queued-1",
  text: "Add link to GitHub repo too",
  createdAt: 1,
};

describe("ComposerQueue", () => {
  afterEach(() => {
    cleanup();
  });

  it("shows direct steer, edit, and cancel actions", () => {
    render(<ComposerQueue queuedMessages={[queuedItem]} steerAvailable />);

    expect(screen.getByLabelText("Steer")).toBeTruthy();
    expect(screen.getByLabelText("Edit")).toBeTruthy();
    expect(screen.getByLabelText("Cancel")).toBeTruthy();
    expect(screen.queryByLabelText("Queue item menu")).toBeNull();
  });

  it("calls steer callback for the selected queued item", () => {
    const onSteerQueued = vi.fn();
    render(
      <ComposerQueue
        queuedMessages={[queuedItem]}
        steerAvailable
        onSteerQueued={onSteerQueued}
      />,
    );

    fireEvent.click(screen.getByLabelText("Steer"));

    expect(onSteerQueued).toHaveBeenCalledTimes(1);
    expect(onSteerQueued).toHaveBeenCalledWith(queuedItem);
  });

  it("disables steer when the active turn cannot accept guidance", () => {
    render(<ComposerQueue queuedMessages={[queuedItem]} steerAvailable={false} />);

    expect(
      (screen.getByLabelText("Steer") as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("calls edit callback for selected queued item", () => {
    const onEditQueued = vi.fn();
    render(<ComposerQueue queuedMessages={[queuedItem]} onEditQueued={onEditQueued} />);

    fireEvent.click(screen.getByLabelText("Edit"));

    expect(onEditQueued).toHaveBeenCalledTimes(1);
    expect(onEditQueued).toHaveBeenCalledWith(queuedItem);
  });

  it("calls delete callback for selected queued item", () => {
    const onDeleteQueued = vi.fn();
    render(<ComposerQueue queuedMessages={[queuedItem]} onDeleteQueued={onDeleteQueued} />);

    fireEvent.click(screen.getByLabelText("Cancel"));

    expect(onDeleteQueued).toHaveBeenCalledTimes(1);
    expect(onDeleteQueued).toHaveBeenCalledWith(queuedItem.id);
  });
});

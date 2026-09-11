/** @vitest-environment jsdom */
import { beforeEach, describe, expect, it, vi } from "vitest";

const sentryInitMock = vi.fn();
const sentryMetricsCountMock = vi.fn();
const renderMock = vi.fn();
const createRootMock = vi.fn(() => ({
  render: renderMock,
}));

vi.mock("@sentry/react", () => ({
  globalHandlersIntegration: vi.fn(() => ({ name: "GlobalHandlers" })),
  init: sentryInitMock,
  makeFetchTransport: vi.fn(),
  metrics: {
    count: sentryMetricsCountMock,
  },
}));

vi.mock("react-dom/client", () => ({
  default: {
    createRoot: createRootMock,
  },
  createRoot: createRootMock,
}));

vi.mock("./App", () => ({
  default: () => null,
}));

describe("main sentry bootstrap", () => {
  beforeEach(() => {
    vi.resetModules();
    sentryInitMock.mockClear();
    sentryMetricsCountMock.mockClear();
    createRootMock.mockClear();
    renderMock.mockClear();
    document.body.innerHTML = '<div id="root"></div>';
  });

  it("keeps upstream telemetry disabled without an explicit DSN", async () => {
    await import("./main");

    expect(sentryInitMock).toHaveBeenCalledTimes(1);
    expect(sentryInitMock).toHaveBeenCalledWith(
      expect.objectContaining({
        dsn: undefined,
        enabled: false,
        release: expect.any(String),
      }),
    );
    expect(sentryMetricsCountMock).not.toHaveBeenCalled();
  });
});

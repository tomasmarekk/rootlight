// Verifies diagnostics render only live, source-free host and daemon results.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  createSupportBundle,
  downloadSupportBundle,
  fetchHealth,
  runQuickDiagnostics,
} from "../src/api/client";
import type { Health, QuickDiagnostics, SupportBundle } from "../src/api/contracts";
import { DiagnosticsPage } from "../src/views/diagnostics-page";

vi.mock("../src/api/client", () => ({
  ApiError: class MockApiError extends Error {
    public readonly code = "request_failed";
  },
  createSupportBundle: vi.fn(),
  downloadSupportBundle: vi.fn(),
  fetchHealth: vi.fn(),
  runQuickDiagnostics: vi.fn(),
}));

const health: Health = {
  webReady: true,
  daemonReady: true,
  protocolVersion: "1.10",
  lifecycle: "ready",
  acceptingOperations: true,
  activeOperations: 2,
  admittedOperations: 3,
  queuedOperations: 1,
  runningOperations: 2,
  activeConnections: 1,
  connectionLimit: 64,
  operationQueueLimit: 128,
  journalHealthy: true,
  catalogSchemaVersion: 4,
  endpointSchemaVersion: 2,
  catalogStatus: "healthy",
  generationStatus: "healthy",
  adapterStatus: "degraded",
  watcherStatus: "not_configured",
  endpointStatus: "healthy",
  resourcePressure: "normal",
};

const diagnostics: QuickDiagnostics = {
  schema: "rootlight.web-quick-diagnostics/1",
  schemaVersion: 1,
  overallStatus: "degraded",
  durationMs: 125,
  checks: [
    {
      name: "catalog",
      outcome: "timed_out",
      durationMs: 125,
      error: {
        code: 12,
        message: "Catalog check timed out",
        retryable: true,
        retryAfterMs: "1000",
      },
    },
  ],
};

const receipt = "s".repeat(43);
const supportBundle: SupportBundle = {
  schema: "rootlight.web-support-bundle/1",
  receipt,
  downloadPath: `/api/v1/diagnostics/support-bundles/${receipt}`,
  archiveBytes: "1024",
  sha256: "a".repeat(64),
  containsSource: false,
  expiresInSeconds: 120,
};

beforeEach(() => {
  vi.mocked(fetchHealth).mockReset();
  vi.mocked(fetchHealth).mockResolvedValue(health);
  vi.mocked(runQuickDiagnostics).mockReset();
  vi.mocked(runQuickDiagnostics).mockResolvedValue(diagnostics);
  vi.mocked(createSupportBundle).mockReset();
  vi.mocked(createSupportBundle).mockResolvedValue(supportBundle);
  vi.mocked(downloadSupportBundle).mockReset();
  vi.mocked(downloadSupportBundle).mockResolvedValue(undefined);
});

describe("DiagnosticsPage", () => {
  it("renders authoritative readiness, capacity, and schema fields", async () => {
    renderPage();

    expect(await screen.findByText("3 / 128")).toBeVisible();
    expect(screen.getByText("1 / 64")).toBeVisible();
    expect(screen.getByText("2 running · 1 queued · 2 active")).toBeVisible();
    expect(screen.getByText("1.10")).toBeVisible();
    expect(screen.getByText("4")).toBeVisible();
    expect(screen.getByText("2")).toBeVisible();
    expect(screen.getAllByText("healthy").length).toBeGreaterThanOrEqual(4);
  });

  it("shows real quick results and downloads only the issued local bundle", async () => {
    renderPage();

    await userEvent.click(screen.getByRole("button", { name: "Quick diagnostics" }));
    expect(await screen.findByText(/Catalog check timed out/u)).toBeVisible();
    expect(screen.getByText("timed out")).toBeVisible();
    expect(runQuickDiagnostics).toHaveBeenCalledOnce();

    await userEvent.click(screen.getByRole("button", { name: "Prepare support bundle" }));
    expect(await screen.findByText("1,024 bytes")).toBeVisible();
    expect(screen.getByText("no")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Verify and download" }));

    expect(downloadSupportBundle).toHaveBeenCalledWith(supportBundle);
    expect(await screen.findByText(/This single-use archive was downloaded/u)).toBeVisible();
  });
});

function renderPage() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <DiagnosticsPage />
    </QueryClientProvider>,
  );
}

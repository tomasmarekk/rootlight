// Verifies daemon readiness transitions refresh source-free route data.

import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { act, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { publishDaemonReconnected } from "../src/api/client";
import type { Health } from "../src/api/contracts";
import { AppShell } from "../src/shell/app-shell";

vi.mock("../src/api/client", () => ({
  fetchHealth: vi.fn(),
  publishDaemonReconnected: vi.fn(),
}));

vi.mock("../src/session/session-context", () => ({
  useSession: () => ({ endSession: vi.fn() }),
}));

const unavailableHealth: Health = {
  webReady: true,
  daemonReady: false,
  protocolVersion: "1.10",
  lifecycle: "ready",
  acceptingOperations: false,
  activeOperations: 0,
  admittedOperations: 0,
  queuedOperations: 0,
  runningOperations: 0,
  activeConnections: 0,
  connectionLimit: 128,
  operationQueueLimit: 256,
  journalHealthy: true,
  catalogSchemaVersion: 4,
  endpointSchemaVersion: 2,
  catalogStatus: "healthy",
  generationStatus: "unavailable",
  adapterStatus: "healthy",
  watcherStatus: "not_configured",
  endpointStatus: "healthy",
  resourcePressure: "unknown",
};
const readyHealth: Health = {
  ...unavailableHealth,
  daemonReady: true,
  acceptingOperations: true,
  activeConnections: 1,
  generationStatus: "healthy",
};

beforeEach(() => {
  vi.mocked(publishDaemonReconnected).mockReset();
});

describe("AppShell", () => {
  it("refreshes route queries when a reachable daemon becomes ready", async () => {
    const routeQuery = vi
      .fn<() => Promise<string>>()
      .mockResolvedValueOnce("stale catalog")
      .mockResolvedValue("ready catalog");
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: {
          retry: false,
          staleTime: Number.POSITIVE_INFINITY,
        },
      },
    });
    queryClient.setQueryData(["health"], unavailableHealth);

    render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/projects"]}>
          <Routes>
            <Route element={<AppShell />}>
              <Route path="/projects" element={<RouteQuery query={routeQuery} />} />
            </Route>
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );

    expect(await screen.findByText("stale catalog")).toBeVisible();
    act(() => {
      queryClient.setQueryData(["health"], readyHealth);
    });

    expect(await screen.findByText("ready catalog")).toBeVisible();
    await waitFor(() => expect(routeQuery).toHaveBeenCalledTimes(2));
    expect(publishDaemonReconnected).toHaveBeenCalledOnce();
  });
});

function RouteQuery({ query }: { query: () => Promise<string> }) {
  const result = useQuery({
    queryKey: ["projects"],
    queryFn: query,
  });
  return <span>{result.data}</span>;
}

// Exercises one-time fragment exchange and the authenticated shell boundary.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => {
  vi.resetModules();
});

afterEach(() => {
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

describe("SessionProvider", () => {
  it("exchanges and removes a valid bootstrap fragment", async () => {
    const { SessionProvider } = await import("../src/session/session-provider");
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ csrfToken: "csrf", idleTtlSeconds: 1_800 }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    window.history.replaceState(null, "", `/#bootstrap=${"a".repeat(43)}`);

    render(
      <TestProviders>
        <SessionProvider>
          <p>Authenticated content</p>
        </SessionProvider>
      </TestProviders>,
    );

    expect(await screen.findByText("Authenticated content")).toBeInTheDocument();
    expect(window.location.hash).toBe("");
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/session/bootstrap");
  });

  it("fails closed when neither a cookie nor a valid bootstrap is accepted", async () => {
    const { SessionProvider } = await import("../src/session/session-provider");
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>().mockResolvedValue(new Response(null, { status: 401 })),
    );

    render(
      <TestProviders>
        <SessionProvider>
          <p>Authenticated content</p>
        </SessionProvider>
      </TestProviders>,
    );

    expect(
      await screen.findByRole("heading", { name: "This Rootlight link is no longer valid" }),
    ).toBeVisible();
    expect(screen.queryByText("Authenticated content")).not.toBeInTheDocument();
  });

  it("keeps CSRF in memory and closes the session after logout", async () => {
    const { SessionProvider } = await import("../src/session/session-provider");
    const { useSession } = await import("../src/session/session-context");
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ csrfToken: "csrf-token", idleTtlSeconds: 1_800 }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      )
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    function SessionConsumer() {
      const { endSession } = useSession();
      return <button onClick={() => void endSession()}>End test session</button>;
    }

    render(
      <TestProviders>
        <SessionProvider>
          <SessionConsumer />
        </SessionProvider>
      </TestProviders>,
    );

    await userEvent.click(await screen.findByRole("button", { name: "End test session" }));
    expect(
      await screen.findByRole("heading", { name: "Rootlight is disconnected from this tab" }),
    ).toBeVisible();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[1]?.[1]).toMatchObject({
      method: "DELETE",
      headers: { "x-rootlight-csrf": "csrf-token" },
    });
  });

  it("clears authenticated content when a later API request reports expiry", async () => {
    const { SessionProvider } = await import("../src/session/session-provider");
    const { fetchHealth } = await import("../src/api/client");
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ csrfToken: "csrf-token", idleTtlSeconds: 1_800 }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      )
      .mockResolvedValueOnce(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", fetchMock);

    function SessionConsumer() {
      return (
        <button onClick={() => void fetchHealth().catch(() => undefined)}>Check health</button>
      );
    }

    render(
      <TestProviders>
        <SessionProvider>
          <SessionConsumer />
        </SessionProvider>
      </TestProviders>,
    );

    await userEvent.click(await screen.findByRole("button", { name: "Check health" }));
    expect(
      await screen.findByRole("heading", { name: "This local session has ended" }),
    ).toBeVisible();
    expect(screen.queryByRole("button", { name: "Check health" })).not.toBeInTheDocument();
  });
});

function TestProviders({ children }: { children: ReactNode }) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return (
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>{children}</MemoryRouter>
    </QueryClientProvider>
  );
}

// Exercises bounded response handling and fail-closed session bootstrap parsing.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => {
  vi.resetModules();
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("browser API client", () => {
  it("restores an existing HttpOnly-cookie session", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValue(jsonResponse({ csrfToken: "csrf", idleTtlSeconds: 1_800 }));
    vi.stubGlobal("fetch", fetchMock);
    const { initializeSession } = await import("../src/api/client");

    await expect(initializeSession()).resolves.toEqual({
      csrfToken: "csrf",
      idleTtlSeconds: 1_800,
    });
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/session");
  });

  it("removes and rejects a malformed bootstrap fragment before fetch", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);
    window.history.replaceState(null, "", "/#bootstrap=too-short");
    const { initializeSession } = await import("../src/api/client");

    await expect(initializeSession()).rejects.toMatchObject({
      status: 401,
      code: "invalid_bootstrap",
    });
    expect(window.location.hash).toBe("");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("rejects oversized, malformed, and unauthorized health responses", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response("{}", {
          status: 200,
          headers: { "content-length": String(1024 * 1024 + 1) },
        }),
      )
      .mockResolvedValueOnce(new Response("not-json", { status: 200 }))
      .mockResolvedValueOnce(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", fetchMock);
    const { fetchHealth } = await import("../src/api/client");

    await expect(fetchHealth()).rejects.toMatchObject({ code: "response_too_large" });
    await expect(fetchHealth()).rejects.toMatchObject({ code: "invalid_response" });
    await expect(fetchHealth()).rejects.toMatchObject({ code: "session_required" });
  });
});

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

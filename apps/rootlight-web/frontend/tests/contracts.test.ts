// Verifies runtime DTO parsing remains strict and fail-closed.

import { describe, expect, it } from "vitest";

import { parseHealth, parseSession } from "../src/api/contracts";

describe("browser API contracts", () => {
  it("accepts the complete health shape", () => {
    expect(parseHealth(healthFixture()).protocolVersion).toBe("1.10");
  });

  it("rejects unknown health states and unsafe counts", () => {
    expect(() => parseHealth({ ...healthFixture(), lifecycle: "invented" })).toThrow();
    expect(() =>
      parseHealth({ ...healthFixture(), activeOperations: Number.MAX_SAFE_INTEGER + 1 }),
    ).toThrow();
    expect(() => parseHealth({ ...healthFixture(), webReady: "yes" })).toThrow();
    expect(() => parseHealth(null)).toThrow();
  });

  it("bounds session credentials", () => {
    expect(parseSession({ csrfToken: "token", idleTtlSeconds: 1_800 })).toEqual({
      csrfToken: "token",
      idleTtlSeconds: 1_800,
    });
    expect(() => parseSession({ csrfToken: "", idleTtlSeconds: 1_800 })).toThrow();
  });
});

function healthFixture() {
  return {
    webReady: true,
    daemonReady: true,
    protocolVersion: "1.10",
    lifecycle: "ready",
    acceptingOperations: true,
    activeOperations: 0,
    queuedOperations: 0,
    runningOperations: 0,
    journalHealthy: true,
    catalogStatus: "healthy",
    generationStatus: "healthy",
    adapterStatus: "healthy",
    watcherStatus: "not_configured",
    endpointStatus: "healthy",
    resourcePressure: "normal",
  };
}

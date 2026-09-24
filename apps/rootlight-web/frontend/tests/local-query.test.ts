// Pins first-party cache identity, invalidation, and session isolation.
// These guarantees protect immutable catalog pages and one-use source capabilities.

import { describe, expect, it, vi } from "vitest";

import { QueryClient } from "../src/query/local-query";

describe("local query cache", () => {
  it("shares structurally equal keys without conflating absent and null fields", () => {
    const client = new QueryClient();
    expect(client.entry(["projects", { state: "ready", after: undefined }])).toBe(
      client.entry(["projects", { after: undefined, state: "ready" }]),
    );
    expect(client.entry(["projects", undefined])).not.toBe(client.entry(["projects", null]));
    client.clear();
  });

  it("refreshes active prefixes while retaining an immutable page after failure", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const entry = client.entry(["projects", { page: 1 }]);
    client.setQueryData(["projects", { page: 1 }], "original");
    const notify = vi.fn();
    const unsubscribe = client.subscribe(entry, notify, 0);
    const request = vi.fn().mockRejectedValue(new Error("daemon unavailable"));
    const unobserve = client.observe(entry, () =>
      client.fetch(
        entry,
        request,
        {
          queryKey: ["projects", { page: 1 }],
          queryFn: request,
        },
        true,
      ),
    );

    await client.invalidateQueries({ queryKey: ["projects"] });
    expect(request).toHaveBeenCalledOnce();
    expect(entry.state.data).toBe("original");
    expect(entry.state.error).toBeInstanceOf(Error);
    expect(notify).toHaveBeenCalled();
    unobserve();
    unsubscribe();
    client.clear();
  });

  it("aborts a pending request and refuses late data after session clearing", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const entry = client.entry(["node-detail", "secret"]);
    let resolveRequest: (value: string) => void = () => undefined;
    const pending = client.fetch(
      entry,
      () =>
        new Promise<string>((resolve) => {
          resolveRequest = resolve;
        }),
      { queryKey: ["node-detail", "secret"], queryFn: () => Promise.resolve("unused") },
    );
    client.clear();
    resolveRequest("stale source capability");

    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    expect(client.entry(["node-detail", "secret"]).state.data).toBeUndefined();
    client.clear();
  });
});

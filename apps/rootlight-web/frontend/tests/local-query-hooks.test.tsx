// Exercises first-party query hooks across shared reads and cursor failures.
// Later-page failures must not erase already shown relationship evidence.

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { QueryClient, useInfiniteQuery, useQuery } from "../src/query/local-query";
import { QueryClientProvider } from "../src/query/query-provider";

describe("local query hooks", () => {
  it("deduplicates mounted consumers of the same immutable key", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const request = vi.fn<() => Promise<string>>().mockResolvedValue("shared page");
    function Consumer() {
      const result = useQuery({ queryKey: ["projects", { page: 1 }], queryFn: request });
      return <span>{result.data}</span>;
    }

    const view = render(
      <QueryClientProvider client={client}>
        <Consumer />
        <Consumer />
      </QueryClientProvider>,
    );
    expect(await screen.findAllByText("shared page")).toHaveLength(2);
    expect(request).toHaveBeenCalledOnce();
    view.unmount();
    client.clear();
  });

  it("keeps the first page after a later cursor fails", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const request = vi.fn(({ pageParam }: { pageParam: string }) => {
      if (pageParam === "1") return Promise.reject(new Error("next page unavailable"));
      return Promise.resolve({ label: "first relationship", next: "1" });
    });
    function Results() {
      const result = useInfiniteQuery({
        queryKey: ["relationships", "generation"],
        queryFn: request,
        initialPageParam: "0",
        getNextPageParam: (page) => page.next,
        retry: false,
      });
      return (
        <>
          <span>{result.data?.pages.map((page) => page.label).join(", ")}</span>
          <span>{result.isError ? "page failed" : "ready"}</span>
          <button onClick={() => void result.fetchNextPage()} type="button">
            More
          </button>
        </>
      );
    }

    const view = render(
      <QueryClientProvider client={client}>
        <Results />
      </QueryClientProvider>,
    );
    expect(await screen.findByText("first relationship")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "More" }));
    await waitFor(() => expect(screen.getByText("page failed")).toBeVisible());
    expect(screen.getByText("first relationship")).toBeVisible();
    expect(request.mock.calls.map(([context]) => context.pageParam)).toEqual(["0", "1"]);
    view.unmount();
    client.clear();
  });
});

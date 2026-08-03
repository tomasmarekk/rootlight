// Verifies exact-generation graph loading, bounded page accumulation, and handle cleanup.

import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchNextGraphPage, openGraphProjection, releaseGraphProjection } from "../src/api/client";
import { useGraphProjection } from "../src/hooks/use-graph-projection";
import { decodeGraphPage } from "../src/workers/graph-decoder-protocol";
import {
  graphGenerationId,
  graphLayoutIdentity,
  graphPageFixture,
  graphProjectionToken,
  graphRepositoryId,
} from "./graph-engine-fixtures";

const decoderMocks = vi.hoisted(() => ({
  decode: vi.fn(),
  dispose: vi.fn(),
}));

vi.mock("../src/api/client", () => ({
  openGraphProjection: vi.fn(),
  fetchNextGraphPage: vi.fn(),
  releaseGraphProjection: vi.fn(),
}));

vi.mock("../src/features/graph/controller/graph-decoder-client", () => ({
  GraphDecoderClient: class {
    decode = decoderMocks.decode;
    dispose = decoderMocks.dispose;
  },
}));

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(openGraphProjection).mockResolvedValue(graphPageFixture(0));
  vi.mocked(fetchNextGraphPage).mockResolvedValue(graphPageFixture(1));
  vi.mocked(releaseGraphProjection).mockResolvedValue({
    schema: "rootlight.web-graph-release/1",
    released: true,
  });
  decoderMocks.decode
    .mockResolvedValueOnce(
      decodeGraphPage({
        type: "decode",
        jobId: 1,
        page: graphPageFixture(0),
        expectedRepositoryId: graphRepositoryId,
        expectedGenerationId: graphGenerationId,
        layoutIdentity: graphLayoutIdentity,
      }),
    )
    .mockResolvedValueOnce(
      decodeGraphPage({
        type: "decode",
        jobId: 2,
        page: graphPageFixture(1),
        expectedRepositoryId: graphRepositoryId,
        expectedGenerationId: graphGenerationId,
        expectedProjectionToken: graphProjectionToken,
        layoutIdentity: graphLayoutIdentity,
      }),
    );
});

describe("useGraphProjection", () => {
  it("loads all bounded pages for an exact generation and releases the projection", async () => {
    const { result, unmount } = renderHook(() =>
      useGraphProjection({
        repositoryId: graphRepositoryId,
        generationId: graphGenerationId,
        view: "architecture",
        relations: ["calls"],
        minimumConfidence: 500,
        budgetProfile: "balanced",
        retryKey: 0,
      }),
    );

    expect(result.current).toMatchObject({ model: null, loading: true, failed: false });
    await waitFor(() => expect(result.current.model?.nodes).toHaveLength(3));

    expect(openGraphProjection).toHaveBeenCalledWith(
      {
        repositoryId: graphRepositoryId,
        generationId: graphGenerationId,
        view: "architecture",
        symbolIds: undefined,
        relations: undefined,
        minConfidence: 500,
        budgetProfile: "balanced",
      },
      expect.any(AbortSignal),
    );
    expect(fetchNextGraphPage).toHaveBeenCalledWith(
      graphProjectionToken,
      graphRepositoryId,
      graphGenerationId,
      expect.any(AbortSignal),
    );
    expect(result.current).toMatchObject({
      loading: false,
      loadingNextPage: false,
      failed: false,
    });
    expect(releaseGraphProjection).toHaveBeenCalledWith(graphProjectionToken);

    unmount();
    expect(decoderMocks.dispose).toHaveBeenCalledOnce();
    expect(releaseGraphProjection).toHaveBeenCalledOnce();
  });

  it("reports a source-free failure state when opening is rejected", async () => {
    vi.mocked(openGraphProjection).mockRejectedValueOnce(new Error("private daemon detail"));
    const { result } = renderHook(() =>
      useGraphProjection({
        repositoryId: graphRepositoryId,
        generationId: graphGenerationId,
        view: "files",
        relations: ["calls"],
        minimumConfidence: 0,
        budgetProfile: "compact",
        retryKey: 0,
      }),
    );

    await waitFor(() => expect(result.current.failed).toBe(true));
    expect(result.current.model).toBeNull();
    expect(releaseGraphProjection).not.toHaveBeenCalled();
  });
});

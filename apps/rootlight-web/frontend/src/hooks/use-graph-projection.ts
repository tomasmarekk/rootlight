// Owns one exact-generation graph projection across HTTP, Worker decoding, and browser memory.

import { useEffect, useState } from "react";

import {
  fetchNextGraphPage,
  openGraphProjection,
  releaseGraphProjection,
  subscribeDaemonReconnected,
} from "../api/client";
import { GraphDecoderClient } from "../features/graph/controller/graph-decoder-client";
import type {
  GraphBudgetProfile,
  GraphRelationKind,
  GraphView,
} from "../features/graph/model/graph-contracts";
import type { GraphLayoutIdentity } from "../features/graph/model/graph-layout";
import type { GraphRenderModel } from "../features/graph/model/graph-model";
import { GraphPageAccumulator } from "../features/graph/model/graph-page-accumulator";

type GraphProjectionInput = {
  repositoryId: string;
  generationId: string;
  view: GraphView;
  selectedSymbolId?: string;
  relations: readonly Exclude<GraphRelationKind, "unknown">[];
  minimumConfidence: number;
  budgetProfile: GraphBudgetProfile;
  retryKey: number;
};

export type GraphProjectionState = {
  model: GraphRenderModel | null;
  loading: boolean;
  loadingNextPage: boolean;
  failed: boolean;
};

const maximumClientMemoryBytes = 64 * 1_024 * 1_024;

/**
 * Opens, validates, accumulates, and releases a projection for one immutable route identity.
 *
 * The browser validates every page again in a Worker. Cleanup aborts late responses and
 * releases the opaque projection handle without exposing it to URL or persistent storage.
 */
export function useGraphProjection(input: GraphProjectionInput): GraphProjectionState {
  const {
    budgetProfile,
    generationId,
    minimumConfidence,
    relations,
    repositoryId,
    retryKey,
    selectedSymbolId,
    view,
  } = input;
  const relationFingerprint = relations.join("\u001f");
  const scopedSelectedSymbolId =
    view === "symbols" || view === "neighborhood" ? selectedSymbolId : undefined;
  const [reconnectRevision, setReconnectRevision] = useState(0);
  useEffect(
    () =>
      subscribeDaemonReconnected(() => {
        setReconnectRevision((current) => current + 1);
      }),
    [],
  );
  const projectionIdentity = [
    repositoryId,
    generationId,
    view,
    scopedSelectedSymbolId ?? "",
    relationFingerprint,
    String(minimumConfidence),
    budgetProfile,
    String(retryKey),
    String(reconnectRevision),
  ].join("\u001e");
  const [state, setState] = useState<GraphProjectionState & { identity: string }>({
    identity: "",
    model: null,
    loading: true,
    loadingNextPage: false,
    failed: false,
  });

  useEffect(() => {
    const abortController = new AbortController();
    const decoder = new GraphDecoderClient();
    const accumulator = new GraphPageAccumulator({
      maximumNodes: 512,
      maximumEdges: 1_000,
      maximumMemoryBytes: maximumClientMemoryBytes,
    });
    const selectedSymbols =
      scopedSelectedSymbolId === undefined ? undefined : [scopedSelectedSymbolId];
    const requestedRelations =
      selectedSymbols === undefined
        ? undefined
        : (relationFingerprint.split("\u001f").filter(Boolean) as Exclude<
            GraphRelationKind,
            "unknown"
          >[]);
    const layoutIdentity: GraphLayoutIdentity = {
      repositoryId,
      generationId,
      view,
      scopeFingerprint: scopedSelectedSymbolId ?? "repository",
      layoutVersion: "atlas-v1",
    };
    let projectionToken: string | undefined;
    let released = false;

    async function releaseProjection() {
      const token = projectionToken;
      if (token === undefined || released) {
        return;
      }
      released = true;
      try {
        await releaseGraphProjection(token);
      } catch {
        // The daemon also owns a bounded TTL, so disconnect cleanup remains deterministic.
      }
    }

    async function loadProjection() {
      try {
        let page = await openGraphProjection(
          {
            repositoryId,
            generationId,
            view,
            symbolIds: selectedSymbols,
            relations: requestedRelations,
            minConfidence: minimumConfidence,
            budgetProfile,
          },
          abortController.signal,
        );
        projectionToken = page.projectionToken;

        for (;;) {
          const prepared = await decoder.decode(
            {
              page,
              expectedRepositoryId: repositoryId,
              expectedGenerationId: generationId,
              expectedProjectionToken: projectionToken,
              layoutIdentity,
            },
            abortController.signal,
          );
          accumulator.append(prepared);
          setState({
            identity: projectionIdentity,
            model: accumulator.snapshot(),
            loading: false,
            loadingNextPage: page.hasNextPage,
            failed: false,
          });
          if (!page.hasNextPage) {
            break;
          }
          page = await fetchNextGraphPage(
            projectionToken,
            repositoryId,
            generationId,
            abortController.signal,
          );
        }
        await releaseProjection();
      } catch (error) {
        if (!(error instanceof DOMException && error.name === "AbortError")) {
          setState({
            identity: projectionIdentity,
            model: null,
            loading: false,
            loadingNextPage: false,
            failed: true,
          });
        }
        await releaseProjection();
      }
    }

    void loadProjection();
    return () => {
      abortController.abort();
      decoder.dispose();
      accumulator.dispose();
      void releaseProjection();
    };
  }, [
    budgetProfile,
    generationId,
    minimumConfidence,
    projectionIdentity,
    relationFingerprint,
    repositoryId,
    retryKey,
    scopedSelectedSymbolId,
    view,
  ]);

  return state.identity === projectionIdentity
    ? state
    : { model: null, loading: true, loadingNextPage: false, failed: false };
}

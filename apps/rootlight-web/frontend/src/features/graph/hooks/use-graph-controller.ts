// Bridges one React viewport container to one imperative Cosmos controller.
// Controller snapshots stay small while typed arrays remain outside React state.

import { useEffect, useLayoutEffect, useMemo, useState, useSyncExternalStore } from "react";

import {
  CosmosGraphController,
  type CosmosGraphControllerOptions,
  type CosmosGraphControllerSnapshot,
} from "../controller/cosmos-graph-controller";
import type { GraphRenderModel } from "../model/graph-model";

/** Inputs for the React controller lifecycle boundary. */
export type UseGraphControllerInput = {
  enabled: boolean;
  model: GraphRenderModel;
  options: CosmosGraphControllerOptions;
};

/** Controller, container binding, and observable state returned to viewport chrome. */
export type UseGraphControllerResult = {
  controller: CosmosGraphController;
  container: HTMLDivElement | null;
  setContainer: (container: HTMLDivElement | null) => void;
  snapshot: CosmosGraphControllerSnapshot;
};

/** Creates, initializes, updates, and disposes exactly one controller per immutable layout identity. */
export function useGraphController(input: UseGraphControllerInput): UseGraphControllerResult {
  const { enabled, model, options } = input;
  const {
    controlledSelection,
    factory,
    layoutIdentity,
    onFallbackRequired,
    onHoverChange,
    onSelectionChange,
    reducedMotion,
    view,
  } = options;
  const [container, setContainer] = useState<HTMLDivElement | null>(null);
  const controller = useMemo(
    () =>
      new CosmosGraphController({
        controlledSelection,
        factory,
        layoutIdentity: {
          repositoryId: layoutIdentity.repositoryId,
          generationId: layoutIdentity.generationId,
          view: layoutIdentity.view,
          scopeFingerprint: layoutIdentity.scopeFingerprint,
          layoutVersion: layoutIdentity.layoutVersion,
        },
        reducedMotion,
        view,
      }),
    [
      controlledSelection,
      factory,
      layoutIdentity.generationId,
      layoutIdentity.layoutVersion,
      layoutIdentity.repositoryId,
      layoutIdentity.scopeFingerprint,
      layoutIdentity.view,
      reducedMotion,
      view,
    ],
  );
  useLayoutEffect(() => {
    controller.updateCallbacks({
      onFallbackRequired,
      onHoverChange,
      onSelectionChange,
    });
  }, [controller, onFallbackRequired, onHoverChange, onSelectionChange]);
  const snapshot = useSyncExternalStore(
    controller.subscribe,
    controller.getSnapshot,
    controller.getSnapshot,
  );

  useEffect(() => {
    if (!enabled || container === null) {
      return;
    }
    void controller.initialize(container);
    return () => {
      controller.dispose();
    };
  }, [container, controller, enabled]);

  useEffect(() => {
    if (!enabled) {
      return;
    }
    controller.applyModel(model);
  }, [controller, enabled, model]);

  return { controller, container, setContainer, snapshot };
}

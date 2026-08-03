// Composes capability detection, the imperative Cosmos host, HUD, toolbar, and text path.
// The canvas is optional; graph meaning and selection remain available through fallback UI.

import { useEffect, useMemo, useRef, useState } from "react";

import type { CosmosGraphFactory } from "../controller/cosmos-graph-controller";
import type { GraphBudgetProfile, GraphView } from "../model/graph-contracts";
import type { GraphLayoutIdentity } from "../model/graph-layout";
import type { GraphRenderModel } from "../model/graph-model";
import { useGraphController } from "../hooks/use-graph-controller";
import { useReducedMotion } from "../hooks/use-reduced-motion";
import { useWebGlCapability, type WebGlCapability } from "../hooks/use-webgl-capability";
import { GraphCompanionList } from "./graph-companion-list";
import { GraphFallback } from "./graph-fallback";
import { GraphHud } from "./graph-hud";
import { GraphToolbar } from "./graph-toolbar";

/** Props for a reusable Rootlight Atlas viewport. */
export type GraphViewportProps = {
  model: GraphRenderModel;
  layoutIdentity: GraphLayoutIdentity;
  view: GraphView;
  budgetProfile: GraphBudgetProfile;
  loadingNextPage?: boolean;
  selectedOrdinals?: readonly number[];
  labelsVisible?: boolean;
  capabilityOverride?: WebGlCapability;
  factory?: CosmosGraphFactory;
  onSelectionChange?: (ordinals: readonly number[]) => void;
  onHoverChange?: (ordinal: number | null) => void;
  onLabelsVisibleChange?: (visible: boolean) => void;
};

/** Renders the GPU viewport when supported and a complete text path otherwise. */
export function GraphViewport(props: GraphViewportProps) {
  const {
    budgetProfile,
    capabilityOverride,
    factory,
    labelsVisible,
    layoutIdentity,
    loadingNextPage,
    model,
    onHoverChange,
    onLabelsVisibleChange,
    onSelectionChange,
    selectedOrdinals,
    view,
  } = props;
  const detectedCapability = useWebGlCapability();
  const capability = capabilityOverride ?? detectedCapability;
  const reducedMotion = useReducedMotion();
  const [fallbackReason, setFallbackReason] = useState<string | null>(null);
  const [fallbackSelection, setFallbackSelection] = useState<readonly number[]>([]);
  const pendingFitOrdinal = useRef<number | null>(null);
  const selectionControlled = selectedOrdinals !== undefined;
  const options = useMemo(
    () => ({
      layoutIdentity,
      view,
      reducedMotion,
      controlledSelection: selectionControlled,
      factory,
      onSelectionChange: (ordinals: readonly number[]) => {
        if (!selectionControlled) {
          setFallbackSelection(ordinals);
        }
        onSelectionChange?.(ordinals);
      },
      onHoverChange,
      onFallbackRequired: (reason: "initialization" | "context_loss") => {
        setFallbackReason(
          reason === "context_loss"
            ? "The graphics context could not be restored safely."
            : "The graphics renderer could not be initialized.",
        );
      },
    }),
    [
      factory,
      layoutIdentity,
      onHoverChange,
      onSelectionChange,
      reducedMotion,
      selectionControlled,
      view,
    ],
  );
  const { controller, setContainer, snapshot } = useGraphController({
    enabled: capability.state === "supported" && fallbackReason === null,
    model,
    options,
  });
  useEffect(() => {
    if (selectedOrdinals === undefined) {
      return;
    }
    controller.syncSelection(selectedOrdinals);
    if (
      pendingFitOrdinal.current !== null &&
      selectedOrdinals.includes(pendingFitOrdinal.current) &&
      capability.state === "supported" &&
      fallbackReason === null
    ) {
      pendingFitOrdinal.current = null;
      controller.fitSelection();
    }
  }, [capability.state, controller, fallbackReason, model.revision, selectedOrdinals]);

  useEffect(() => {
    if (labelsVisible !== undefined) {
      controller.setLabelsVisible(labelsVisible);
    }
  }, [controller, labelsVisible]);

  const selectFromCompanion = (ordinal: number, fit: boolean) => {
    if (
      capability.state === "supported" &&
      fallbackReason === null &&
      snapshot.lifecycle !== "failed" &&
      snapshot.lifecycle !== "disposed"
    ) {
      controller.setSelection([ordinal]);
    } else {
      if (selectedOrdinals === undefined) {
        setFallbackSelection([ordinal]);
      }
      onSelectionChange?.([ordinal]);
    }
    if (fit && capability.state === "supported" && fallbackReason === null) {
      if (selectionControlled) {
        pendingFitOrdinal.current = ordinal;
      } else {
        controller.fitSelection();
      }
    }
  };

  if (capability.state === "unsupported" || fallbackReason !== null) {
    return (
      <GraphFallback
        model={model}
        budgetProfile={budgetProfile}
        reason={
          fallbackReason ??
          (capability.state === "unsupported"
            ? capability.reason
            : "The graphics renderer is unavailable.")
        }
        selectedOrdinals={selectedOrdinals ?? fallbackSelection}
        onSelect={selectFromCompanion}
      />
    );
  }

  return (
    <section
      className="graph-viewport"
      aria-label="Code graph"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          controller.resetSelection();
        }
      }}
    >
      <GraphToolbar
        capability={capability}
        labelsVisible={labelsVisible ?? snapshot.labelsVisible}
        simulation={snapshot.simulation}
        hasSelection={(selectedOrdinals ?? snapshot.selectedOrdinals).length > 0}
        onFitAll={() => {
          controller.fitAll();
        }}
        onFitSelection={() => {
          controller.fitSelection();
        }}
        onResetSelection={() => {
          controller.resetSelection();
        }}
        onLabelsVisibleChange={(visible) => {
          if (labelsVisible === undefined) {
            controller.setLabelsVisible(visible);
          }
          onLabelsVisibleChange?.(visible);
        }}
        onPauseSimulation={() => {
          controller.pauseSimulation();
        }}
        onResumeSimulation={() => {
          controller.resumeSimulation();
        }}
      />
      <GraphHud model={model} budgetProfile={budgetProfile} loadingNextPage={loadingNextPage} />
      {capability.state === "checking" ? (
        <p role="status">Checking graphics support…</p>
      ) : (
        <div
          ref={setContainer}
          className="graph-viewport__canvas"
          aria-hidden="true"
          data-lifecycle={snapshot.lifecycle}
        />
      )}
      <GraphCompanionList
        model={model}
        selectedOrdinals={selectedOrdinals ?? snapshot.selectedOrdinals}
        onSelect={selectFromCompanion}
      />
    </section>
  );
}

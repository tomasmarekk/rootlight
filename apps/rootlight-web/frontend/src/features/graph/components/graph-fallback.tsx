// Keeps graph exploration usable when WebGL or Cosmos initialization is unavailable.
// It preserves authoritative completeness and stable ordinal selection through text UI.

import type { GraphBudgetProfile } from "../model/graph-contracts";
import type { GraphRenderModel } from "../model/graph-model";
import { GraphCompanionList } from "./graph-companion-list";
import { GraphHud } from "./graph-hud";

/** Props for the complete source-free text fallback. */
export type GraphFallbackProps = {
  model: GraphRenderModel;
  budgetProfile: GraphBudgetProfile;
  reason: string;
  selectedOrdinals: readonly number[];
  overlayOrdinals?: readonly number[];
  onSelect: (ordinal: number, fit: boolean) => void;
  onRetry?: () => void;
};

/** Renders projection status and searchable graph nodes without allocating a canvas. */
export function GraphFallback(props: GraphFallbackProps) {
  return (
    <section className="graph-fallback" aria-labelledby="graph-fallback-title">
      <h2 id="graph-fallback-title">Graphical view is unavailable</h2>
      <p>{props.reason}</p>
      {props.onRetry === undefined ? null : (
        <button type="button" onClick={props.onRetry}>
          Retry graphics
        </button>
      )}
      <GraphHud model={props.model} budgetProfile={props.budgetProfile} />
      <GraphCompanionList
        model={props.model}
        selectedOrdinals={props.selectedOrdinals}
        overlayOrdinals={props.overlayOrdinals}
        onSelect={props.onSelect}
      />
    </section>
  );
}

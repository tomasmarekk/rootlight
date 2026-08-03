// Exposes bounded camera, selection, labels, and simulation controls for Atlas.
// Native controls preserve keyboard behavior in both GPU and fallback modes.

import type { GraphSimulationState } from "../controller/cosmos-graph-controller";
import type { WebGlCapability } from "../hooks/use-webgl-capability";

/** Props for reusable graph camera and display controls. */
export type GraphToolbarProps = {
  capability: WebGlCapability;
  labelsVisible: boolean;
  simulation: GraphSimulationState;
  hasSelection: boolean;
  onFitAll: () => void;
  onFitSelection: () => void;
  onResetSelection: () => void;
  onLabelsVisibleChange: (visible: boolean) => void;
  onPauseSimulation: () => void;
  onResumeSimulation: () => void;
};

/** Renders accessible graph actions without exposing unsafe renderer configuration. */
export function GraphToolbar(props: GraphToolbarProps) {
  return (
    <nav className="graph-toolbar" aria-label="Graph controls">
      <button
        type="button"
        onClick={props.onFitAll}
        disabled={props.capability.state !== "supported"}
      >
        Fit all
      </button>
      <button
        type="button"
        onClick={props.onFitSelection}
        disabled={props.capability.state !== "supported" || !props.hasSelection}
      >
        Fit selection
      </button>
      <button type="button" onClick={props.onResetSelection} disabled={!props.hasSelection}>
        Clear selection
      </button>
      <label>
        <input
          id="graph-labels-visible"
          name="graph-labels-visible"
          type="checkbox"
          checked={props.labelsVisible}
          onChange={(event) => {
            props.onLabelsVisibleChange(event.currentTarget.checked);
          }}
        />
        Labels
      </label>
      <details>
        <summary>Advanced</summary>
        {props.simulation === "running" ? (
          <button type="button" onClick={props.onPauseSimulation}>
            Pause layout
          </button>
        ) : (
          <button
            type="button"
            onClick={props.onResumeSimulation}
            disabled={props.capability.state !== "supported"}
          >
            Resume layout
          </button>
        )}
      </details>
      <output aria-label="WebGL status">{webGlStatusLabel(props.capability)}</output>
    </nav>
  );
}

function webGlStatusLabel(capability: WebGlCapability) {
  if (capability.state === "checking") {
    return "Checking WebGL 2";
  }
  return capability.state === "supported" ? "WebGL 2 ready" : "WebGL 2 unavailable";
}

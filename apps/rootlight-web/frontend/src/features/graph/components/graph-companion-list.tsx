// Provides a bounded, source-free text path through the current projection.
// It supports the same stable ordinal selection and focus callbacks as the canvas.

import { useMemo, useState } from "react";

import type { BrowserGraphNode } from "../model/graph-contracts";
import type { GraphRenderModel } from "../model/graph-model";

/** Props for the accessible graph companion list. */
export type GraphCompanionListProps = {
  model: GraphRenderModel;
  selectedOrdinals: readonly number[];
  overlayOrdinals?: readonly number[];
  onSelect: (ordinal: number, fit: boolean) => void;
};

type CompanionRow = {
  ordinal: number;
  node: BrowserGraphNode;
};

/** Renders the bounded projection as a complete screen-reader and keyboard-accessible list. */
export function GraphCompanionList({
  model,
  overlayOrdinals = [],
  selectedOrdinals,
  onSelect,
}: GraphCompanionListProps) {
  const [query, setQuery] = useState("");
  const rows = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase();
    const filtered: CompanionRow[] = [];
    for (let index = 0; index < model.nodes.length; index += 1) {
      const node = model.nodes[index];
      const ordinal = model.nodeOrdinals[index];
      if (node === undefined || ordinal === undefined) {
        continue;
      }
      const haystack = `${node.label}\u001f${node.path ?? ""}`.toLocaleLowerCase();
      if (normalizedQuery.length === 0 || haystack.includes(normalizedQuery)) {
        filtered.push({ ordinal, node });
      }
    }
    return filtered.sort((left, right) => {
      return left.node.label.localeCompare(right.node.label) || left.ordinal - right.ordinal;
    });
  }, [model, query]);
  const selected = new Set(selectedOrdinals);
  const overlay = new Set(overlayOrdinals);

  return (
    <section className="graph-companion" aria-labelledby="graph-companion-title">
      <h3 id="graph-companion-title">Graph companion</h3>
      <label>
        Search visible nodes
        <input
          id="graph-companion-search"
          name="graph-companion-search"
          type="search"
          value={query}
          onChange={(event) => {
            setQuery(event.currentTarget.value);
          }}
        />
      </label>
      <p role="status">
        {rows.length.toLocaleString()} of {model.nodes.length.toLocaleString()} returned nodes
      </p>
      <div className="graph-companion__viewport" tabIndex={-1}>
        <ul aria-label="Visible graph nodes" className="graph-companion__rows">
          {rows.map(({ ordinal, node }) => (
            <li key={ordinal}>
              <button
                type="button"
                aria-pressed={selected.has(ordinal)}
                data-impact-overlay={overlay.has(ordinal) || undefined}
                onClick={() => {
                  onSelect(ordinal, false);
                }}
                onDoubleClick={() => {
                  onSelect(ordinal, true);
                }}
              >
                <strong>{node.label}</strong>
                <span>{node.kind}</span>
                <span>{node.path ?? "No path context"}</span>
                <span>{formatConfidence(node.confidence)} confidence</span>
                {overlay.has(ordinal) ? (
                  <span className="graph-companion__impact">Change impact</span>
                ) : null}
              </button>
            </li>
          ))}
        </ul>
      </div>
      {rows.length === 0 ? <p>No returned nodes match this local filter.</p> : null}
    </section>
  );
}

function formatConfidence(confidence: number) {
  return `${String(Math.round(confidence / 10))}%`;
}

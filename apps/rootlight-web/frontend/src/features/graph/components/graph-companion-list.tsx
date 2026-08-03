// Provides a bounded, virtualized, source-free text path through the current projection.
// It supports the same stable ordinal selection and focus callbacks as the canvas.

import { useMemo, useState, type UIEvent } from "react";

import type { BrowserGraphNode } from "../model/graph-contracts";
import type { GraphRenderModel } from "../model/graph-model";

/** Props for the accessible graph companion list. */
export type GraphCompanionListProps = {
  model: GraphRenderModel;
  selectedOrdinals: readonly number[];
  onSelect: (ordinal: number, fit: boolean) => void;
  height?: number;
  rowHeight?: number;
};

type CompanionRow = {
  ordinal: number;
  node: BrowserGraphNode;
};

/** Renders only the visible text rows while preserving screen-reader and keyboard controls. */
export function GraphCompanionList({
  model,
  selectedOrdinals,
  onSelect,
  height = 320,
  rowHeight = 54,
}: GraphCompanionListProps) {
  const [query, setQuery] = useState("");
  const [scrollTop, setScrollTop] = useState(0);
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
  const overscan = 4;
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan);
  const visibleCount = Math.ceil(height / rowHeight) + overscan * 2;
  const visibleRows = rows.slice(start, start + visibleCount);
  const selected = new Set(selectedOrdinals);

  const handleScroll = (event: UIEvent<HTMLDivElement>) => {
    setScrollTop(event.currentTarget.scrollTop);
  };

  return (
    <section className="graph-companion" aria-labelledby="graph-companion-title">
      <h3 id="graph-companion-title">Graph companion</h3>
      <label>
        Search visible nodes
        <input
          type="search"
          value={query}
          onChange={(event) => {
            setQuery(event.currentTarget.value);
            setScrollTop(0);
          }}
        />
      </label>
      <p role="status">
        {rows.length.toLocaleString()} of {model.nodes.length.toLocaleString()} returned nodes
      </p>
      <div
        className="graph-companion__viewport"
        style={{ height, overflowY: "auto" }}
        onScroll={handleScroll}
        tabIndex={-1}
      >
        <ul
          aria-label="Visible graph nodes"
          className="graph-companion__rows"
          style={{ height: rows.length * rowHeight, margin: 0, position: "relative" }}
        >
          {visibleRows.map(({ ordinal, node }, index) => (
            <li
              key={ordinal}
              style={{
                height: rowHeight,
                left: 0,
                position: "absolute",
                right: 0,
                top: (start + index) * rowHeight,
              }}
            >
              <button
                type="button"
                aria-pressed={selected.has(ordinal)}
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

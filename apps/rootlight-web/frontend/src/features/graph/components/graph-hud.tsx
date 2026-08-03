// Presents authoritative projection completeness and count context above Atlas.
// Server completeness stays distinct from optional client-visible filtering.

import type { GraphBudgetProfile } from "../model/graph-contracts";
import type { GraphRenderModel } from "../model/graph-model";

/** Props for the compact graph projection status display. */
export type GraphHudProps = {
  model: GraphRenderModel;
  budgetProfile: GraphBudgetProfile;
  visibleNodeCount?: number;
  loadingNextPage?: boolean;
};

/** Renders exact returned, matching, visibility, budget, and completeness state. */
export function GraphHud({
  model,
  budgetProfile,
  visibleNodeCount = model.nodes.length,
  loadingNextPage = false,
}: GraphHudProps) {
  return (
    <section className="graph-hud" aria-label="Graph projection status">
      <dl>
        <div>
          <dt>Visible nodes</dt>
          <dd>{visibleNodeCount.toLocaleString()}</dd>
        </div>
        <div>
          <dt>Returned nodes</dt>
          <dd>
            {model.returnedNodes} of {model.totalMatchingNodes}
          </dd>
        </div>
        <div>
          <dt>Returned relations</dt>
          <dd>
            {model.returnedEdges} of {model.totalMatchingEdges}
          </dd>
        </div>
        <div>
          <dt>Budget</dt>
          <dd>{budgetProfile}</dd>
        </div>
      </dl>
      <p className={`graph-completeness graph-completeness--${model.completeness.state}`}>
        {completenessLabel(model.completeness.state)}
      </p>
      {loadingNextPage ? (
        <p className="graph-page-loading" role="status">
          Loading the next bounded page…
        </p>
      ) : null}
    </section>
  );
}

function completenessLabel(state: GraphRenderModel["completeness"]["state"]) {
  switch (state) {
    case "complete":
      return "Complete projection";
    case "truncated":
      return "Partial projection — server limit reached";
    case "unsupported_partial":
      return "Partial projection — some relations are unsupported";
    case "indeterminate":
      return "Projection completeness is indeterminate";
  }
}

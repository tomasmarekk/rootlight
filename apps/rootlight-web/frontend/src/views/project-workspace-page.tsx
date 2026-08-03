// Resolves an immutable generation before mounting the bounded Atlas workspace.

import { Button } from "@heroui/react/button";
import { useQuery } from "@tanstack/react-query";
import { Activity, ArrowLeft, Database, RefreshCw, RotateCcw, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, useLocation, useParams, useSearchParams } from "react-router";

import { ApiError, fetchProjectDetail } from "../api/client";
import type { ProjectDetail } from "../api/contracts";
import { WorkspaceResizer } from "../components/workspace-resizer";
import { GraphViewport } from "../features/graph/components/graph-viewport";
import type { BrowserGraphNode, GraphView } from "../features/graph/model/graph-contracts";
import {
  EvidenceInspector,
  EvidenceInspectorBoundary,
} from "../features/inspector/components/evidence-inspector";
import { useGraphProjection } from "../hooks/use-graph-projection";
import { useWorkspaceRailWidth, workspaceRailWidthClass } from "../hooks/use-workspace-rail-width";
import { parseCatalogLocationState } from "../routing/catalog-location-state";
import {
  defaultProjectWorkspaceState,
  parseProjectWorkspaceState,
  projectGraphRelationKinds,
  serializeProjectWorkspaceState,
  type ProjectWorkspaceState,
} from "../routing/project-workspace-state";

export function ProjectWorkspacePage() {
  const { repositoryId } = useParams();
  const location = useLocation();
  const catalogLocationState = useMemo(
    () => parseCatalogLocationState(location.state),
    [location.state],
  );
  const [searchParameters] = useSearchParams();
  const workspaceState = parseProjectWorkspaceState(searchParameters);
  const project = useQuery({
    queryKey: ["project", repositoryId, workspaceState.generation],
    queryFn: ({ signal }) => {
      if (repositoryId === undefined) {
        throw new Error("Repository identity is unavailable");
      }
      return fetchProjectDetail(repositoryId, workspaceState.generation, signal);
    },
    enabled: repositoryId !== undefined,
  });

  if (project.isPending) {
    return (
      <div className="workspace-loading" aria-busy="true" aria-label="Loading project generation">
        <RefreshCw className="spin" size={24} aria-hidden="true" />
        Resolving immutable generation
      </div>
    );
  }
  if (project.isError || repositoryId === undefined) {
    const invalidIdentifier = project.error instanceof ApiError && project.error.status === 400;
    return (
      <div className="workspace-error" role="alert">
        <TriangleAlert size={28} aria-hidden="true" />
        <h1>
          {invalidIdentifier
            ? "Project identifier is invalid"
            : "Project generation is unavailable"}
        </h1>
        <p>
          {invalidIdentifier
            ? "The route does not contain a canonical Rootlight repository identifier."
            : "The daemon did not return a correlated immutable project status."}
        </p>
        <Button size="sm" variant="primary" onPress={() => void project.refetch()}>
          Retry
        </Button>
        <Link className="back-link" state={catalogLocationState} to="/projects">
          Return to projects
        </Link>
      </div>
    );
  }

  return (
    <ProjectWorkspace
      repositoryId={repositoryId}
      detail={project.data}
      catalogLocationState={catalogLocationState}
    />
  );
}

function ProjectWorkspace({
  catalogLocationState,
  detail,
  repositoryId,
}: {
  catalogLocationState: ReturnType<typeof parseCatalogLocationState>;
  detail: ProjectDetail;
  repositoryId: string;
}) {
  const [searchParameters, setSearchParameters] = useSearchParams();
  const workspaceState = useMemo(
    () => parseProjectWorkspaceState(searchParameters),
    [searchParameters],
  );
  const canonicalParameters = useMemo(
    () => serializeProjectWorkspaceState(workspaceState),
    [workspaceState],
  );
  const { width: railWidth, setWidth: setRailWidth } = useWorkspaceRailWidth();
  const [retryKey, setRetryKey] = useState(0);
  const [impactSymbolIds, setImpactSymbolIds] = useState<readonly string[]>([]);
  const graph = useGraphProjection({
    repositoryId,
    generationId: detail.resolvedGenerationId,
    view: workspaceState.view,
    selectedSymbolId: workspaceState.selected?.startsWith("sym1_")
      ? workspaceState.selected
      : undefined,
    relations: workspaceState.relations,
    minimumConfidence: workspaceState.minConfidence,
    budgetProfile: workspaceState.budgetProfile,
    retryKey,
  });

  useEffect(() => {
    if (canonicalParameters.toString() !== searchParameters.toString()) {
      setSearchParameters(canonicalParameters, {
        replace: true,
        state: catalogLocationState,
      });
    }
  }, [canonicalParameters, catalogLocationState, searchParameters, setSearchParameters]);

  const selectedOrdinals = useMemo(() => {
    const model = graph.model;
    const selected = workspaceState.selected;
    if (model === null || selected === undefined) {
      return [];
    }
    const index = model.nodes.findIndex((node) => node.stableId === selected);
    const ordinal = index < 0 ? undefined : model.nodeOrdinals[index];
    return ordinal === undefined ? [] : [ordinal];
  }, [graph.model, workspaceState.selected]);
  const selectedNode = useMemo(() => {
    const selected = workspaceState.selected;
    if (selected === undefined) {
      return undefined;
    }
    return (
      graph.model?.nodes.find((node) => node.stableId === selected) ??
      selectionPlaceholder(selected)
    );
  }, [graph.model, workspaceState.selected]);
  const impactOrdinals = useMemo(() => {
    const model = graph.model;
    if (model === null || impactSymbolIds.length === 0) {
      return [];
    }
    const impacted = new Set(impactSymbolIds);
    const ordinals: number[] = [];
    for (let index = 0; index < model.nodes.length; index += 1) {
      if (impacted.has(model.nodes[index]?.stableId ?? "")) {
        const ordinal = model.nodeOrdinals[index];
        if (ordinal !== undefined) {
          ordinals.push(ordinal);
        }
      }
    }
    return ordinals;
  }, [graph.model, impactSymbolIds]);
  const canOpenSeededView = workspaceState.selected?.startsWith("sym1_") === true;

  const updateWorkspace = useCallback(
    (update: (current: ProjectWorkspaceState) => ProjectWorkspaceState, replace = false) => {
      setSearchParameters(serializeProjectWorkspaceState(update(workspaceState)), {
        replace,
        state: catalogLocationState,
      });
    },
    [catalogLocationState, setSearchParameters, workspaceState],
  );

  const closeInspector = useCallback(() => {
    updateWorkspace((current) => ({
      ...current,
      selected: undefined,
      view:
        current.view === "symbols" || current.view === "neighborhood"
          ? "architecture"
          : current.view,
    }));
  }, [updateWorkspace]);

  const openInspectorNode = useCallback(
    (stableId: string) => {
      updateWorkspace((current) => ({ ...current, selected: stableId }));
    },
    [updateWorkspace],
  );

  const updateImpactOverlay = useCallback((stableIds: readonly string[]) => {
    setImpactSymbolIds(stableIds);
  }, []);

  function selectGraphNode(ordinals: readonly number[]) {
    const ordinal = ordinals[0];
    const model = graph.model;
    const pointIndex =
      ordinal === undefined || model === null ? undefined : model.ordinalToPointIndex.get(ordinal);
    const stableId =
      pointIndex === undefined || model === null ? undefined : model.nodes[pointIndex]?.stableId;
    updateWorkspace((current) => ({
      ...current,
      selected: stableId,
      view:
        stableId?.startsWith("sym1_") === true ||
        (current.view !== "symbols" && current.view !== "neighborhood")
          ? current.view
          : "architecture",
    }));
  }

  return (
    <div className="workspace-frame">
      <header className="project-header">
        <div className="project-header__identity">
          <Link className="back-link" state={catalogLocationState} to="/projects">
            <ArrowLeft size={14} aria-hidden="true" />
            Projects
          </Link>
          <div>
            <h1>{detail.displayName}</h1>
            {detail.alias === null ? null : <span>{detail.alias}</span>}
            <code title={detail.repositoryId}>{detail.repositoryId}</code>
          </div>
        </div>
        <div className="project-header__status" aria-label="Project status">
          <span className={`state-label state-label--${publicationTone(detail.publicationState)}`}>
            {humanize(detail.publicationState)}
          </span>
          <span>{humanize(detail.lifecycleState)}</span>
        </div>
        <div className="project-header__actions">
          <label>
            <span>Generation</span>
            <select
              id="workspace-generation"
              name="workspace-generation"
              value={workspaceState.generation}
              onChange={(event) => {
                const generation = event.currentTarget.value;
                updateWorkspace((current) => ({
                  ...current,
                  generation,
                  view: "architecture",
                  selected: undefined,
                }));
              }}
            >
              <option value="active">Follow active · {shortId(detail.activeGenerationId)}</option>
              {workspaceState.generation === "active" ? null : (
                <option value={detail.resolvedGenerationId}>
                  Pinned · {shortId(detail.resolvedGenerationId)}
                </option>
              )}
            </select>
          </label>
          <Button
            size="sm"
            variant="ghost"
            onPress={() => {
              updateWorkspace(() => ({
                ...defaultProjectWorkspaceState,
                generation: workspaceState.generation,
              }));
            }}
          >
            <RotateCcw size={15} aria-hidden="true" />
            Reset view
          </Button>
        </div>
      </header>

      <div
        className={`workspace-grid ${workspaceRailWidthClass(railWidth)}${
          selectedNode === undefined ? "" : " workspace-grid--inspector"
        }`}
      >
        <aside className="workspace-rail" id="project-information">
          <p className="eyebrow">Exact generation</p>
          <h2>{shortId(detail.resolvedGenerationId)}</h2>
          <p className="workspace-generation-note">
            {detail.resolvedGenerationId === detail.activeGenerationId
              ? "This projection matches the active generation."
              : "Historical projection pinned independently of the active pointer."}
          </p>
          <dl className="generation-facts">
            <div>
              <dt>Publication</dt>
              <dd>{humanize(detail.publicationState)}</dd>
            </div>
            <div>
              <dt>Lifecycle</dt>
              <dd>{humanize(detail.lifecycleState)}</dd>
            </div>
            <div>
              <dt>Structural freshness</dt>
              <dd>{humanize(detail.structuralFreshness)}</dd>
            </div>
            <div>
              <dt>Semantic freshness</dt>
              <dd>{humanize(detail.semanticFreshness)}</dd>
            </div>
          </dl>

          <fieldset className="workspace-view-selector">
            <legend>Graph view</legend>
            {(["architecture", "files", "symbols", "neighborhood"] as const).map((view) => (
              <label key={view} title={viewAvailability(view, canOpenSeededView)}>
                <input
                  type="radio"
                  name="graph-view"
                  value={view}
                  checked={workspaceState.view === view}
                  disabled={(view === "symbols" || view === "neighborhood") && !canOpenSeededView}
                  onChange={() => {
                    updateWorkspace((current) => ({ ...current, view }));
                  }}
                />
                <span>{humanize(view)}</span>
              </label>
            ))}
            {!canOpenSeededView ? (
              <small>Select a returned symbol to enable bounded symbol views.</small>
            ) : null}
          </fieldset>

          <div className="workspace-filter-grid">
            <label>
              <span>Minimum confidence</span>
              <select
                id="workspace-minimum-confidence"
                name="workspace-minimum-confidence"
                value={workspaceState.minConfidence}
                onChange={(event) => {
                  const minimumConfidence = Number(
                    event.currentTarget.value,
                  ) as ProjectWorkspaceState["minConfidence"];
                  updateWorkspace((current) => ({
                    ...current,
                    minConfidence: minimumConfidence,
                  }));
                }}
              >
                <option value={0}>All · 0+</option>
                <option value={250}>25% · 250+</option>
                <option value={500}>50% · 500+</option>
                <option value={750}>75% · 750+</option>
              </select>
            </label>
            <label>
              <span>Projection budget</span>
              <select
                id="workspace-projection-budget"
                name="workspace-projection-budget"
                value={workspaceState.budgetProfile}
                onChange={(event) => {
                  const budgetProfile = event.currentTarget
                    .value as ProjectWorkspaceState["budgetProfile"];
                  updateWorkspace((current) => ({ ...current, budgetProfile }));
                }}
              >
                <option value="compact">Compact</option>
                <option value="balanced">Balanced</option>
                <option value="expanded">Expanded</option>
              </select>
            </label>
          </div>

          {workspaceState.view === "symbols" || workspaceState.view === "neighborhood" ? (
            <details className="workspace-relations">
              <summary>Relation families · {workspaceState.relations.length}</summary>
              <fieldset>
                <legend>Server-side relation filter</legend>
                {projectGraphRelationKinds.map((relation) => (
                  <label key={relation}>
                    <input
                      id={`workspace-relation-${relation}`}
                      name="workspace-relations"
                      type="checkbox"
                      checked={workspaceState.relations.includes(relation)}
                      onChange={(event) => {
                        const next = event.currentTarget.checked
                          ? [...workspaceState.relations, relation]
                          : workspaceState.relations.filter((value) => value !== relation);
                        if (next.length > 0) {
                          updateWorkspace((current) => ({ ...current, relations: next }));
                        }
                      }}
                    />
                    {humanize(relation)}
                  </label>
                ))}
              </fieldset>
            </details>
          ) : null}

          <div className="workspace-section">
            <div className="workspace-section__heading">
              <Database size={15} aria-hidden="true" />
              <h3>Coverage</h3>
            </div>
            {detail.coverage.length === 0 ? (
              <p>No coverage groups were reported.</p>
            ) : (
              <ul className="coverage-list">
                {detail.coverage.map((coverage) => (
                  <li key={coverage.language}>
                    <strong>{coverage.language}</strong>
                    <span>
                      {humanize(coverage.tier)} · {humanize(coverage.status)}
                    </span>
                    <small>
                      {coverage.indexedFiles} / {coverage.discoveredFiles} files
                    </small>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="workspace-section">
            <div className="workspace-section__heading">
              <Activity size={15} aria-hidden="true" />
              <h3>Recent operations</h3>
            </div>
            {detail.operations.length === 0 ? (
              <p>No bounded operations were reported.</p>
            ) : (
              <ul className="operation-list">
                {detail.operations.map((operation) => (
                  <li key={operation.operationId}>
                    <div>
                      <strong>{humanize(operation.kind)}</strong>
                      <span
                        className={`state-label state-label--${operationTone(operation.state)}`}
                      >
                        {humanize(operation.state)}
                      </span>
                    </div>
                    <code title={operation.operationId}>{shortId(operation.operationId)}</code>
                    <small>
                      {operation.completedUnits} / {operation.totalUnits} units
                    </small>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </aside>
        <WorkspaceResizer width={railWidth} onWidthChange={setRailWidth} />

        <section
          aria-label="Project graph workspace"
          className="graph-workspace"
          id="project-graph"
          tabIndex={-1}
        >
          <a className="skip-graph-link" href="#graph-companion-title">
            Skip graph canvas
          </a>
          <div className="graph-scope">
            <span>Repository</span>
            <strong>{detail.displayName}</strong>
            <span aria-hidden="true">/</span>
            <span>{humanize(workspaceState.view)}</span>
            {workspaceState.selected === undefined ? null : (
              <>
                <span aria-hidden="true">/</span>
                <code>{shortId(workspaceState.selected)}</code>
              </>
            )}
            {impactOrdinals.length === 0 ? null : (
              <strong className="impact-overlay-label">
                Impact overlay · {impactOrdinals.length}
              </strong>
            )}
          </div>

          {graph.loading && graph.model === null ? (
            <div className="graph-phase" aria-busy="true" role="status">
              <RefreshCw className="spin" size={24} aria-hidden="true" />
              <strong>Preparing bounded {humanize(workspaceState.view)} projection</strong>
              <span>{shortId(detail.resolvedGenerationId)}</span>
            </div>
          ) : null}
          {graph.failed ? (
            <div className="graph-phase graph-phase--error" role="alert">
              <TriangleAlert size={24} aria-hidden="true" />
              <strong>Graph projection is unavailable</strong>
              <span>
                The daemon rejected, disconnected from, or could not validate this bounded view.
              </span>
              <Button
                size="sm"
                variant="primary"
                onPress={() => {
                  setRetryKey((current) => current + 1);
                }}
              >
                Retry projection
              </Button>
            </div>
          ) : null}
          {graph.model === null ? null : (
            <GraphViewport
              key={graph.model.projectionToken}
              model={graph.model}
              layoutIdentity={{
                repositoryId,
                generationId: detail.resolvedGenerationId,
                view: workspaceState.view,
                scopeFingerprint: workspaceState.selected ?? "repository",
                layoutVersion: "atlas-v1",
              }}
              view={workspaceState.view}
              budgetProfile={workspaceState.budgetProfile}
              loadingNextPage={graph.loadingNextPage}
              selectedOrdinals={selectedOrdinals}
              overlayOrdinals={impactOrdinals}
              labelsVisible={workspaceState.labels}
              onSelectionChange={selectGraphNode}
              onLabelsVisibleChange={(labels) => {
                updateWorkspace((current) => ({ ...current, labels }), true);
              }}
            />
          )}
        </section>
        {selectedNode === undefined ? null : (
          <EvidenceInspectorBoundary
            key={`${detail.resolvedGenerationId}:${selectedNode.stableId}`}
            onClose={closeInspector}
          >
            <EvidenceInspector
              repositoryId={repositoryId}
              generationId={detail.resolvedGenerationId}
              selectedNode={selectedNode}
              relations={workspaceState.relations}
              minimumConfidence={workspaceState.minConfidence}
              onClose={closeInspector}
              onOpenNode={openInspectorNode}
              onImpactChange={updateImpactOverlay}
            />
          </EvidenceInspectorBoundary>
        )}
      </div>
    </div>
  );
}

function shortId(identifier: string) {
  return identifier.length > 18 ? `${identifier.slice(0, 13)}…${identifier.slice(-4)}` : identifier;
}

function humanize(value: string) {
  return value.replaceAll("_", " ");
}

function operationTone(state: string) {
  if (state === "succeeded") {
    return "success";
  }
  if (state === "queued" || state === "running" || state === "cancelling") {
    return "warning";
  }
  return "neutral";
}

function publicationTone(state: string) {
  return state === "published" ? "success" : "warning";
}

function viewAvailability(view: GraphView, hasSymbolSeed: boolean) {
  if ((view === "symbols" || view === "neighborhood") && !hasSymbolSeed) {
    return "Select a symbol from the current projection first";
  }
  return `${humanize(view)} graph`;
}

function selectionPlaceholder(stableId: string): BrowserGraphNode {
  const idKind = stableId.startsWith("sym1_") ? "symbol" : "file";
  return {
    ordinal: 0,
    stableId,
    idKind,
    label: shortId(stableId),
    path: null,
    kind: idKind,
    confidence: 0,
    generated: null,
    community: null,
    component: null,
    symbolCount: null,
    fanIn: null,
    fanOut: null,
    hotspotScore: null,
    evidence: "unknown",
  };
}

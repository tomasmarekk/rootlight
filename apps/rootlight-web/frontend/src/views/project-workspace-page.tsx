// Resolves the exact immutable generation before any graph request is admitted.

import { Button } from "@heroui/react/button";
import { useQuery } from "@tanstack/react-query";
import {
  Activity,
  ArrowLeft,
  Database,
  Network,
  RefreshCw,
  RotateCcw,
  TriangleAlert,
} from "lucide-react";
import { Link, useLocation, useParams, useSearchParams } from "react-router";

import { ApiError, fetchProjectDetail } from "../api/client";
import { parseCatalogLocationState } from "../routing/catalog-location-state";

export function ProjectWorkspacePage() {
  const { repositoryId } = useParams();
  const location = useLocation();
  const catalogLocationState = parseCatalogLocationState(location.state);
  const [searchParameters, setSearchParameters] = useSearchParams();
  const generation = searchParameters.get("generation") ?? "active";
  const project = useQuery({
    queryKey: ["project", repositoryId, generation],
    queryFn: ({ signal }) => {
      if (repositoryId === undefined) {
        throw new Error("Repository identity is unavailable");
      }
      return fetchProjectDetail(repositoryId, generation, signal);
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
  if (project.isError) {
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

  const detail = project.data;
  return (
    <div className="workspace-frame">
      <header className="project-header">
        <div>
          <Link className="back-link" state={catalogLocationState} to="/projects">
            <ArrowLeft size={14} aria-hidden="true" />
            Projects
          </Link>
          <h1>{detail.displayName}</h1>
          <code>{detail.repositoryId}</code>
        </div>
        <div className="project-header__actions">
          <label>
            <span>Generation</span>
            <select
              value={generation}
              onChange={(event) => {
                const next = new URLSearchParams(searchParameters);
                next.set("generation", event.currentTarget.value);
                setSearchParameters(next);
              }}
            >
              <option value="active">Active · {shortId(detail.activeGenerationId)}</option>
              {generation === "active" ? null : (
                <option value={detail.resolvedGenerationId}>
                  Pinned · {shortId(detail.resolvedGenerationId)}
                </option>
              )}
            </select>
          </label>
          <Button size="sm" variant="ghost">
            <RotateCcw size={15} aria-hidden="true" />
            Reset view
          </Button>
        </div>
      </header>
      <div className="workspace-grid">
        <aside className="workspace-rail">
          <p className="eyebrow">Generation overview</p>
          <h2>{shortId(detail.resolvedGenerationId)}</h2>
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

          <div className="workspace-section">
            <h3>Projection controls</h3>
            <p>
              Architecture, file, and symbol filters activate after graph capability negotiation.
            </p>
          </div>
        </aside>
        <section className="graph-placeholder" aria-label="Graph visualization">
          <Network size={30} aria-hidden="true" />
          <h2>Bounded graph projection not loaded</h2>
          <p>
            Generation {shortId(detail.resolvedGenerationId)} is pinned. The canvas opens only after
            the daemon advertises the graph projection capability.
          </p>
        </section>
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

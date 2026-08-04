// Presents immutable repository catalog pages without mixing daemon snapshots.

import { Button } from "@heroui/react/button";
import { Card } from "@heroui/react/card";
import { useMutation, useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import {
  Archive,
  ArrowLeft,
  ArrowRight,
  CircleCheck,
  FolderGit2,
  FolderPlus,
  Pencil,
  RefreshCw,
  Search,
  Trash2,
  TriangleAlert,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation } from "react-router";

import {
  createClientRequestId,
  deleteProject,
  fetchProjects,
  renameProject,
  submitProjectIndex,
} from "../api/client";
import type { ProjectCatalogPage, ProjectLifecycleFilter, ProjectSummary } from "../api/contracts";
import { AddProjectDialog, type ProjectIndexSelection } from "../components/add-project-dialog";
import { NativeDialog } from "../components/native-dialog";
import { PageHeading } from "../components/page-heading";
import { SessionOperationList } from "../components/session-operation-list";
import { StatusCard } from "../components/status-card";
import { useOperations } from "../operations/operation-context";
import {
  createCatalogLocationState,
  parseCatalogLocationState,
  type CatalogCursorState,
  type CatalogLocationState,
} from "../routing/catalog-location-state";

const pageSize = 50;

function hasUnsafeAliasCharacter(alias: string) {
  for (const character of alias) {
    const codePoint = character.codePointAt(0) ?? 0;
    if (codePoint <= 0x1f || codePoint === 0x7f || character === "/" || character === "\\") {
      return true;
    }
  }
  return false;
}

export function ProjectsPage() {
  const location = useLocation();
  const queryClient = useQueryClient();
  const { register } = useOperations();
  const [restored] = useState(() => parseCatalogLocationState(location.state)?.catalog);
  const [searchInput, setSearchInput] = useState(restored?.searchInput ?? "");
  const [query, setQuery] = useState(restored?.query ?? "");
  const [stateFilter, setStateFilter] = useState<ProjectLifecycleFilter | "all">(
    restored?.stateFilter ?? "all",
  );
  const [history, setHistory] = useState<CatalogCursorState[]>(restored?.history ?? []);
  const [addProjectOpen, setAddProjectOpen] = useState(false);
  const [focusOperationId, setFocusOperationId] = useState<string>();
  const cursor = history.at(-1);
  const isInitialSearchEffect = useRef(true);
  const hasPendingInitialSearch = useRef(searchInput.trim() !== query);

  useEffect(() => {
    if (isInitialSearchEffect.current) {
      isInitialSearchEffect.current = false;
      if (!hasPendingInitialSearch.current) {
        return;
      }
    }
    const timer = window.setTimeout(() => {
      setQuery(searchInput.trim());
      setHistory([]);
    }, 250);
    return () => window.clearTimeout(timer);
  }, [searchInput]);

  const catalog = useQuery({
    queryKey: [
      "projects",
      {
        query,
        stateFilter,
        snapshot: cursor?.snapshot,
        after: cursor?.after,
        sortVersion: cursor?.sortVersion,
      },
    ],
    queryFn: ({ signal }) =>
      fetchProjects(
        {
          pageSize,
          query: query.length === 0 ? undefined : query,
          states: stateFilter === "all" ? undefined : [stateFilter],
          snapshot: cursor?.snapshot,
          after: cursor?.after,
          sortVersion: cursor?.sortVersion,
        },
        signal,
      ),
  });

  const summary = useMemo(() => summarize(catalog.data?.projects ?? []), [catalog.data?.projects]);
  const hasFilter = query.length > 0 || stateFilter !== "all";
  const catalogLocationState = createCatalogLocationState({
    searchInput,
    query,
    stateFilter,
    history,
  });

  async function submitSelection(selection: ProjectIndexSelection) {
    const requestId = createClientRequestId();
    const admission = await submitProjectIndex({
      rootCapability: selection.rootCapability,
      mode: selection.mode,
      clientRequestId: requestId,
    });
    register(admission, requestId);
    setFocusOperationId(admission.operationId);
  }

  async function refreshAfterMutation() {
    setHistory([]);
    await queryClient.invalidateQueries({ queryKey: ["projects"] });
  }

  return (
    <div className="content-container">
      <PageHeading
        eyebrow="Local repository catalog"
        title="Projects"
        subtitle="Structural and semantic indexes available to this Rootlight daemon."
        actions={
          <>
            <Button
              isDisabled={catalog.isFetching}
              size="sm"
              variant="ghost"
              onPress={() => void catalog.refetch()}
            >
              <RefreshCw size={15} aria-hidden="true" />
              Refresh
            </Button>
            <Button size="sm" variant="primary" onPress={() => setAddProjectOpen(true)}>
              <FolderPlus size={15} aria-hidden="true" />
              Add project
            </Button>
          </>
        }
      />

      <section className="metrics-grid" aria-label="Project catalog summary">
        <StatusCard
          icon={<Archive size={17} />}
          label="Catalog"
          value={catalog.data?.totalCount ?? "—"}
          detail={
            catalog.data?.totalCount === null ? "Exact total is unavailable" : "Indexed roots"
          }
        />
        <StatusCard
          icon={<CircleCheck size={17} />}
          label="Ready on page"
          value={catalog.data === undefined ? "—" : String(summary.ready)}
          detail="Current page only"
        />
        <StatusCard
          icon={<RefreshCw size={17} />}
          label="Indexing on page"
          value={catalog.data === undefined ? "—" : String(summary.indexing)}
          detail="Current page only"
        />
        <StatusCard
          icon={<TriangleAlert size={17} />}
          label="Attention on page"
          value={catalog.data === undefined ? "—" : String(summary.attention)}
          detail="Current page only"
        />
      </section>

      <SessionOperationList
        focusOperationId={focusOperationId}
        onFocused={() => setFocusOperationId(undefined)}
      />

      <section className="catalog-panel" aria-labelledby="catalog-heading">
        <div className="catalog-toolbar">
          <div>
            <h2 id="catalog-heading">Repository indexes</h2>
            <p>Search and open an immutable published generation.</p>
          </div>
          <div className="catalog-controls">
            <label className="state-filter">
              <span>State</span>
              <select
                id="project-state-filter"
                name="project-state-filter"
                value={stateFilter}
                onChange={(event) => {
                  setStateFilter(event.currentTarget.value as ProjectLifecycleFilter | "all");
                  setHistory([]);
                }}
              >
                <option value="all">All states</option>
                <option value="ready">Ready</option>
                <option value="indexing">Indexing</option>
                <option value="degraded">Degraded</option>
                <option value="corrupt">Corrupt</option>
                <option value="migration_required">Migration required</option>
                <option value="rebuild_required">Rebuild required</option>
              </select>
            </label>
            <label className="search-control">
              <Search size={16} aria-hidden="true" />
              <span className="sr-only">Search projects</span>
              <input
                id="project-search"
                name="project-search"
                type="search"
                aria-label="Search projects"
                maxLength={256}
                placeholder="Search projects or paths"
                value={searchInput}
                onChange={(event) => setSearchInput(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    setQuery(event.currentTarget.value.trim());
                    setHistory([]);
                  } else if (event.key === "Escape") {
                    event.preventDefault();
                    setSearchInput("");
                    setQuery("");
                    setHistory([]);
                  }
                }}
              />
              <kbd aria-hidden="true">/</kbd>
            </label>
          </div>
        </div>

        <CatalogContent
          catalog={catalog}
          hasFilter={hasFilter}
          locationState={catalogLocationState}
          onChanged={refreshAfterMutation}
          onAddProject={() => setAddProjectOpen(true)}
          onClearFilters={() => {
            setSearchInput("");
            setQuery("");
            setStateFilter("all");
            setHistory([]);
          }}
        />

        {catalog.data !== undefined && (history.length > 0 || catalog.data.nextAfter !== null) ? (
          <nav className="catalog-pagination" aria-label="Project catalog pages">
            <Button
              isDisabled={history.length === 0 || catalog.isFetching}
              size="sm"
              variant="ghost"
              onPress={() => setHistory((current) => current.slice(0, -1))}
            >
              <ArrowLeft size={14} aria-hidden="true" />
              Previous
            </Button>
            <span aria-live="polite">Page {history.length + 1}</span>
            <Button
              isDisabled={catalog.data.nextAfter === null || catalog.isFetching}
              size="sm"
              variant="ghost"
              onPress={() => {
                const page = catalog.data;
                const after = page.nextAfter;
                if (after !== null) {
                  setHistory((current) => [
                    ...current,
                    {
                      snapshot: page.snapshot,
                      after,
                      sortVersion: page.sortVersion,
                    },
                  ]);
                }
              }}
            >
              Next
              <ArrowRight size={14} aria-hidden="true" />
            </Button>
          </nav>
        ) : null}
      </section>

      <AddProjectDialog
        isOpen={addProjectOpen}
        onOpenChange={setAddProjectOpen}
        onSubmit={submitSelection}
      />
    </div>
  );
}

function CatalogContent({
  catalog,
  hasFilter,
  locationState,
  onAddProject,
  onChanged,
  onClearFilters,
}: {
  catalog: UseQueryResult<ProjectCatalogPage>;
  hasFilter: boolean;
  locationState: CatalogLocationState;
  onAddProject: () => void;
  onChanged: () => Promise<void>;
  onClearFilters: () => void;
}) {
  if (catalog.isPending) {
    return (
      <div className="project-list" aria-label="Loading project catalog" aria-busy="true">
        {[0, 1, 2].map((value) => (
          <div className="project-card project-card--skeleton" key={value} />
        ))}
      </div>
    );
  }
  if (catalog.isError && catalog.data === undefined) {
    return (
      <Card className="empty-state-card empty-state-card--error" variant="secondary">
        <Card.Content>
          <div className="empty-state-icon" aria-hidden="true">
            <TriangleAlert size={22} />
          </div>
          <p className="eyebrow">Catalog unavailable</p>
          <Card.Title>Rootlight could not load this catalog snapshot</Card.Title>
          <Card.Description>
            The daemon may be reconnecting. No local catalog data was replaced.
          </Card.Description>
          <Button size="sm" variant="primary" onPress={() => void catalog.refetch()}>
            <RefreshCw size={15} aria-hidden="true" />
            Retry
          </Button>
        </Card.Content>
      </Card>
    );
  }
  const staleNotice = catalog.isError ? (
    <div className="catalog-stale-notice" role="status">
      <TriangleAlert size={15} aria-hidden="true" />
      Refresh failed. The previous immutable catalog page remains visible.
    </div>
  ) : null;
  if (catalog.data.projects.length === 0) {
    return (
      <>
        {staleNotice}
        <Card className="empty-state-card" variant="secondary">
          <Card.Content>
            <div className="empty-state-icon" aria-hidden="true">
              {hasFilter ? <Search size={22} /> : <FolderPlus size={22} />}
            </div>
            <p className="eyebrow">{hasFilter ? "No matching indexes" : "Catalog ready"}</p>
            <Card.Title>
              {hasFilter ? "No projects match these filters" : "No projects have been loaded yet"}
            </Card.Title>
            <Card.Description>
              {hasFilter
                ? "Clear the current search and state filter to return to the complete catalog."
                : "Add a local repository to build a bounded structural index. Source stays on this machine and is accessed only through the Rootlight daemon."}
            </Card.Description>
            {hasFilter ? (
              <Button size="sm" variant="ghost" onPress={onClearFilters}>
                Clear filters
              </Button>
            ) : (
              <Button size="sm" variant="primary" onPress={onAddProject}>
                <FolderPlus size={15} aria-hidden="true" />
                Add your first project
              </Button>
            )}
          </Card.Content>
        </Card>
      </>
    );
  }
  return (
    <>
      {staleNotice}
      <div className="project-list" aria-live="polite">
        {catalog.data.projects.map((project) => (
          <ProjectCard
            key={project.repositoryId}
            locationState={locationState}
            project={project}
            onChanged={onChanged}
          />
        ))}
      </div>
    </>
  );
}

function ProjectCard({
  locationState,
  onChanged,
  project,
}: {
  locationState: CatalogLocationState;
  onChanged: () => Promise<void>;
  project: ProjectSummary;
}) {
  const [dialog, setDialog] = useState<"delete" | "rename" | null>(null);
  const effectiveName = project.alias ?? project.displayName;
  const [alias, setAlias] = useState(effectiveName);
  const rename = useMutation({
    mutationFn: (nextAlias: string) => renameProject(project.repositoryId, nextAlias),
  });
  const remove = useMutation({
    mutationFn: () => deleteProject(project.repositoryId),
  });
  const coverage = aggregateCoverage(project);
  const mutationPending = rename.isPending || remove.isPending;
  const normalizedAlias = alias.trim();
  const aliasInvalid =
    normalizedAlias.length === 0 ||
    normalizedAlias.length > 256 ||
    hasUnsafeAliasCharacter(normalizedAlias);

  function openDialog(nextDialog: "delete" | "rename") {
    rename.reset();
    remove.reset();
    setAlias(effectiveName);
    setDialog(nextDialog);
  }

  async function submitRename() {
    if (aliasInvalid) {
      return;
    }
    await rename.mutateAsync(normalizedAlias);
    setDialog(null);
    await onChanged();
  }

  async function submitDelete() {
    await remove.mutateAsync();
    setDialog(null);
    await onChanged();
  }

  return (
    <article
      className={`project-card${project.activeGenerationId === null ? " project-card--disabled" : ""}`}
      aria-label={effectiveName}
    >
      <div className="project-card__body">
        <div className="project-card__identity">
          <div className="project-card__icon" aria-hidden="true">
            <FolderGit2 size={19} />
          </div>
          <div>
            <h3>
              <button
                className="project-card__name"
                type="button"
                aria-label={`Rename ${effectiveName}`}
                title="Rename project"
                onClick={() => openDialog("rename")}
              >
                {effectiveName}
                <Pencil size={12} aria-hidden="true" />
              </button>
            </h3>
            <p className="project-card__path" title={project.rootPath ?? undefined}>
              {project.rootPath ?? "Source path unavailable for this legacy index"}
            </p>
          </div>
          <span className={`state-badge state-badge--${project.lifecycleState}`}>
            {humanize(project.lifecycleState)}
          </span>
        </div>
        <div className="project-card__metadata">
          <span>
            {project.languages.length === 0 ? "No language data" : project.languages.join(", ")}
          </span>
          <span>{project.generationCount} generations</span>
          <span title={project.activeGenerationId ?? undefined}>
            {project.activeGenerationId === null
              ? "No active generation"
              : `Active ${shortId(project.activeGenerationId)}`}
          </span>
          <span>Structural {humanize(project.structuralFreshness)}</span>
          <span>Semantic {humanize(project.semanticFreshness)}</span>
        </div>
        <progress
          aria-label={coverage.label}
          className="coverage-track"
          max={100}
          value={coverage.percent}
        />
        <div className="project-card__coverage">
          <span>{coverage.label}</span>
          <span>{project.coverage.length} coverage groups</span>
        </div>
      </div>
      <div className="project-card__actions">
        <button className="project-card__delete" type="button" onClick={() => openDialog("delete")}>
          <Trash2 size={13} aria-hidden="true" />
          Delete
        </button>
        {project.activeGenerationId === null ? null : (
          <Link
            className="project-card__open"
            state={locationState}
            to={`/projects/${encodeURIComponent(project.repositoryId)}?generation=${encodeURIComponent(project.activeGenerationId)}`}
          >
            Open project
            <ArrowRight size={13} aria-hidden="true" />
          </Link>
        )}
      </div>
      <ProjectMutationDialog
        alias={alias}
        aliasInvalid={aliasInvalid}
        effectiveName={effectiveName}
        error={rename.isError || remove.isError}
        isOpen={dialog !== null}
        mode={dialog ?? "rename"}
        pending={mutationPending}
        rootPath={project.rootPath}
        onAliasChange={setAlias}
        onDismiss={() => {
          if (!mutationPending) {
            setDialog(null);
          }
        }}
        onSubmit={() => {
          const submission = dialog === "delete" ? submitDelete() : submitRename();
          void submission.catch(() => undefined);
        }}
      />
    </article>
  );
}

function ProjectMutationDialog({
  alias,
  aliasInvalid,
  effectiveName,
  error,
  isOpen,
  mode,
  pending,
  rootPath,
  onAliasChange,
  onDismiss,
  onSubmit,
}: {
  alias: string;
  aliasInvalid: boolean;
  effectiveName: string;
  error: boolean;
  isOpen: boolean;
  mode: "delete" | "rename";
  pending: boolean;
  rootPath: string | null;
  onAliasChange: (alias: string) => void;
  onDismiss: () => void;
  onSubmit: () => void;
}) {
  const headingId = mode === "rename" ? "rename-project-heading" : "delete-project-heading";
  return (
    <NativeDialog
      ariaLabelledBy={headingId}
      className="project-mutation-modal"
      isDismissable={!pending}
      isOpen={isOpen}
      onDismiss={onDismiss}
    >
      <header data-slot="modal-header">
        <div>
          <p className="eyebrow">{mode === "rename" ? "Project identity" : "Remove local index"}</p>
          <h2 id={headingId}>{mode === "rename" ? "Rename project" : "Delete project"}</h2>
        </div>
        {pending ? null : (
          <button
            aria-label={`Close ${mode} project dialog`}
            className="native-dialog__close"
            type="button"
            onClick={onDismiss}
          >
            <X size={18} aria-hidden="true" />
          </button>
        )}
      </header>
      <div data-slot="modal-body">
        {mode === "rename" ? (
          <div className="project-mutation-field">
            <label htmlFor="project-name">Project name</label>
            <input
              autoFocus
              id="project-name"
              maxLength={256}
              name="project-name"
              value={alias}
              onChange={(event) => onAliasChange(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !aliasInvalid && !pending) {
                  event.preventDefault();
                  onSubmit();
                }
              }}
            />
            <small>The source folder name and contents stay unchanged.</small>
          </div>
        ) : (
          <div className="project-delete-warning">
            <TriangleAlert size={20} aria-hidden="true" />
            <div>
              <strong>Delete “{effectiveName}” from Rootlight?</strong>
              <p>
                Its local index, generations, and Rootlight metadata will be removed. The source
                directory{rootPath === null ? "" : ` at ${rootPath}`} will not be changed.
              </p>
            </div>
          </div>
        )}
        {error ? (
          <div className="project-mutation-error" role="alert">
            Rootlight could not complete this change. The project may be busy; try again after its
            current operation finishes.
          </div>
        ) : null}
      </div>
      <footer data-slot="modal-footer">
        <Button isDisabled={pending} variant="ghost" onPress={onDismiss}>
          Cancel
        </Button>
        <Button
          className={mode === "delete" ? "project-delete-confirm" : undefined}
          isDisabled={pending || (mode === "rename" && aliasInvalid)}
          variant="primary"
          onPress={onSubmit}
        >
          {pending
            ? mode === "rename"
              ? "Saving"
              : "Deleting"
            : mode === "rename"
              ? "Save name"
              : "Delete Rootlight data"}
        </Button>
      </footer>
    </NativeDialog>
  );
}

function summarize(projects: ProjectSummary[]) {
  return projects.reduce(
    (summary, project) => {
      if (project.lifecycleState === "ready") {
        summary.ready += 1;
      } else if (project.lifecycleState === "indexing") {
        summary.indexing += 1;
      } else {
        summary.attention += 1;
      }
      return summary;
    },
    { ready: 0, indexing: 0, attention: 0 },
  );
}

function aggregateCoverage(project: ProjectSummary) {
  let discovered = 0n;
  let indexed = 0n;
  for (const coverage of project.coverage) {
    discovered += BigInt(coverage.discoveredFiles);
    indexed += BigInt(coverage.indexedFiles);
  }
  if (discovered === 0n) {
    return { percent: 0, label: "Coverage not reported" };
  }
  const percent = Number((indexed * 100n) / discovered);
  return {
    percent: Math.min(percent, 100),
    label: `${String(percent)}% indexed coverage`,
  };
}

function humanize(value: string) {
  return value.replaceAll("_", " ");
}

function shortId(identifier: string) {
  return identifier.length > 18 ? `${identifier.slice(0, 13)}…${identifier.slice(-4)}` : identifier;
}

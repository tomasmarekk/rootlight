// Keeps local directory selection behind short-lived server capabilities.

import { Button } from "@heroui/react/button";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  ArrowLeft,
  ChevronRight,
  Folder,
  FolderOpen,
  HardDrive,
  Info,
  Search,
  ShieldCheck,
  TriangleAlert,
  X,
} from "lucide-react";
import { useRef, useState, type KeyboardEvent } from "react";

import {
  browseFilesystem,
  fetchFilesystemRoots,
  openFilesystemPath,
  preflightFilesystemIndex,
} from "../api/client";
import type {
  FilesystemBrowsePage,
  FilesystemDirectory,
  IndexMode,
  IndexPreflight,
} from "../api/contracts";
import { NativeDialog } from "./native-dialog";

const browserPageSize = 64;
const maximumVisibleDirectories = 256;

export type ProjectIndexSelection = {
  rootCapability: string;
  mode: IndexMode;
  displayLabel: string;
};

export function AddProjectDialog({
  isOpen,
  onOpenChange,
  onSubmit,
}: {
  isOpen: boolean;
  onOpenChange: (open: boolean) => void;
  onSubmit: (selection: ProjectIndexSelection) => Promise<void>;
}) {
  const [page, setPage] = useState<FilesystemBrowsePage>();
  const [filter, setFilter] = useState("");
  const [directPath, setDirectPath] = useState("");
  const [mode, setMode] = useState<IndexMode>("auto");
  const [preflight, setPreflight] = useState<IndexPreflight>();
  const [submitError, setSubmitError] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  const roots = useQuery({
    queryKey: ["filesystem-roots"],
    queryFn: ({ signal }) => fetchFilesystemRoots(signal),
    enabled: isOpen,
    gcTime: 0,
    staleTime: 0,
    retry: false,
  });
  const browse = useMutation({
    mutationFn: (request: Parameters<typeof browseFilesystem>[0]) => browseFilesystem(request),
  });
  const openPath = useMutation({
    mutationFn: (path: string) => openFilesystemPath(path),
  });
  const preflightRoot = useMutation({
    mutationFn: ({ browseToken, selectedMode }: { browseToken: string; selectedMode: IndexMode }) =>
      preflightFilesystemIndex(browseToken, selectedMode),
  });

  const loading = browse.isPending || openPath.isPending || preflightRoot.isPending;
  const requestFailed =
    roots.isError || browse.isError || openPath.isError || preflightRoot.isError;

  async function loadDirectory(
    browseToken: string,
    action: { type: "current" } | { type: "parent" } | { type: "child"; name: string },
    options: { append?: boolean; cursor?: string; appliedFilter?: string } = {},
  ) {
    const nextPage = await browse.mutateAsync({
      browseToken,
      action,
      pageSize: browserPageSize,
      cursor: options.cursor,
      filter: options.appliedFilter,
    });
    setPreflight(undefined);
    setPage((current) =>
      options.append && current?.browseToken === nextPage.browseToken
        ? {
            ...nextPage,
            directories: [...current.directories, ...nextPage.directories].slice(
              0,
              maximumVisibleDirectories,
            ),
            nextCursor:
              current.directories.length + nextPage.directories.length >= maximumVisibleDirectories
                ? null
                : nextPage.nextCursor,
          }
        : nextPage,
    );
  }

  function runRequest(request: Promise<void>) {
    // React Query owns the visible error state; this prevents event-handler promise rejections.
    void request.catch(() => undefined);
  }

  async function openDirectPath() {
    if (directPath.trim().length === 0) {
      return;
    }
    const opened = await openPath.mutateAsync(directPath);
    setFilter("");
    await loadDirectory(opened.browseToken, { type: "current" });
  }

  async function reviewSelection() {
    if (page === undefined) {
      return;
    }
    const result = await preflightRoot.mutateAsync({
      browseToken: page.browseToken,
      selectedMode: mode,
    });
    setPreflight(result);
  }

  async function submitSelection() {
    if (preflight === undefined) {
      return;
    }
    setSubmitting(true);
    setSubmitError(false);
    try {
      await onSubmit({
        rootCapability: preflight.rootCapability,
        mode: preflight.selectedMode,
        displayLabel: preflight.normalizedDisplayLabel,
      });
      reset();
      onOpenChange(false);
    } catch {
      setSubmitError(true);
    } finally {
      setSubmitting(false);
    }
  }

  function changeOpen(open: boolean) {
    if (open || !submitting) {
      if (!open) {
        reset();
      }
      onOpenChange(open);
    }
  }

  function reset() {
    setPage(undefined);
    setFilter("");
    setDirectPath("");
    setMode("auto");
    setPreflight(undefined);
    setSubmitError(false);
    setSubmitting(false);
  }

  const headingId = "add-project-dialog-heading";
  return (
    <NativeDialog
      ariaLabelledBy={headingId}
      className="add-project-modal"
      isDismissable={!submitting}
      isOpen={isOpen}
      onDismiss={() => changeOpen(false)}
    >
      <header data-slot="modal-header">
        <div>
          <p className="eyebrow">Local repository onboarding</p>
          <h2 id={headingId} data-slot="modal-heading">
            Add a project
          </h2>
          <p className="add-project-modal__subtitle">
            Select an existing local folder. Rootlight never uploads its source.
          </p>
        </div>
        {submitting ? null : (
          <button
            aria-label="Close add project dialog"
            className="native-dialog__close"
            type="button"
            onClick={() => changeOpen(false)}
          >
            <X size={18} aria-hidden="true" />
          </button>
        )}
      </header>

      <div data-slot="modal-body">
        {preflight === undefined ? (
          <DirectoryBrowser
            directPath={directPath}
            filter={filter}
            loading={loading}
            page={page}
            roots={roots.data?.roots ?? []}
            rootsLoading={roots.isPending}
            selectedMode={mode}
            onDirectPathChange={setDirectPath}
            onFilterChange={setFilter}
            onModeChange={setMode}
            onOpenDirectPath={() => runRequest(openDirectPath())}
            onOpenRoot={(token) => runRequest(loadDirectory(token, { type: "current" }))}
            onNavigate={(token) => {
              setFilter("");
              runRequest(loadDirectory(token, { type: "current" }));
            }}
            onOpenChild={(name) => {
              if (page !== undefined) {
                setFilter("");
                runRequest(loadDirectory(page.browseToken, { type: "child", name }));
              }
            }}
            onApplyFilter={() => {
              if (page !== undefined) {
                runRequest(
                  loadDirectory(
                    page.browseToken,
                    { type: "current" },
                    {
                      appliedFilter: filter.trim() || undefined,
                    },
                  ),
                );
              }
            }}
            onLoadMore={() => {
              if (page?.nextCursor !== null && page?.nextCursor !== undefined) {
                runRequest(
                  loadDirectory(
                    page.browseToken,
                    { type: "current" },
                    {
                      append: true,
                      cursor: page.nextCursor,
                      appliedFilter: filter.trim() || undefined,
                    },
                  ),
                );
              }
            }}
          />
        ) : (
          <PreflightReview preflight={preflight} />
        )}

        {requestFailed ? (
          <div className="add-project-error" role="alert">
            <TriangleAlert size={16} aria-hidden="true" />
            The local folder request could not be completed. Check the selection and try again.
          </div>
        ) : null}
        {submitError ? (
          <div className="add-project-error" role="alert">
            <TriangleAlert size={16} aria-hidden="true" />
            Rootlight could not admit this index operation. The folder capability may have expired;
            reopen the selection and retry.
          </div>
        ) : null}
        <div className="local-security-note">
          <ShieldCheck size={17} aria-hidden="true" />
          <div>
            <strong>Local, capability-bound access</strong>
            <span>
              Rootlight reads only the selected root on this machine. Source is not uploaded, and
              symlinks or paths outside the root are not followed implicitly.
            </span>
          </div>
        </div>
      </div>

      <footer data-slot="modal-footer">
        {preflight === undefined ? (
          <>
            <Button isDisabled={loading} variant="ghost" onPress={() => changeOpen(false)}>
              Cancel
            </Button>
            <Button
              isDisabled={page === undefined || loading}
              variant="primary"
              onPress={() => runRequest(reviewSelection())}
            >
              {preflightRoot.isPending ? "Checking folder" : "Select this folder"}
            </Button>
          </>
        ) : (
          <>
            <Button
              isDisabled={submitting}
              variant="ghost"
              onPress={() => {
                setPreflight(undefined);
                setSubmitError(false);
              }}
            >
              <ArrowLeft size={14} aria-hidden="true" />
              Change selection
            </Button>
            <Button
              isDisabled={
                submitting || !preflight.selectable || !preflight.daemonAcceptingOperations
              }
              variant="primary"
              onPress={() => void submitSelection()}
            >
              {submitting ? "Starting index" : "Start detached index"}
            </Button>
          </>
        )}
      </footer>
    </NativeDialog>
  );
}

function DirectoryBrowser({
  directPath,
  filter,
  loading,
  page,
  roots,
  rootsLoading,
  selectedMode,
  onApplyFilter,
  onDirectPathChange,
  onFilterChange,
  onLoadMore,
  onModeChange,
  onNavigate,
  onOpenChild,
  onOpenDirectPath,
  onOpenRoot,
}: {
  directPath: string;
  filter: string;
  loading: boolean;
  page: FilesystemBrowsePage | undefined;
  roots: { label: string; browseToken: string; readable: boolean; selectable: boolean }[];
  rootsLoading: boolean;
  selectedMode: IndexMode;
  onApplyFilter: () => void;
  onDirectPathChange: (path: string) => void;
  onFilterChange: (filter: string) => void;
  onLoadMore: () => void;
  onModeChange: (mode: IndexMode) => void;
  onNavigate: (token: string) => void;
  onOpenChild: (name: string) => void;
  onOpenDirectPath: () => void;
  onOpenRoot: (token: string) => void;
}) {
  return (
    <div className="directory-browser">
      <aside className="directory-browser__roots" aria-label="Filesystem roots">
        <h3>Locations</h3>
        {rootsLoading ? <p aria-live="polite">Loading local roots…</p> : null}
        {roots.map((root) => (
          <button
            key={root.browseToken}
            type="button"
            disabled={!root.readable || !root.selectable || loading}
            onClick={() => onOpenRoot(root.browseToken)}
          >
            <HardDrive size={15} aria-hidden="true" />
            <span>{root.label}</span>
          </button>
        ))}
        <div className="direct-path">
          <label htmlFor="direct-project-path">Direct absolute path</label>
          <input
            id="direct-project-path"
            name="direct-project-path"
            maxLength={8_192}
            placeholder="Enter an absolute path"
            value={directPath}
            onChange={(event) => onDirectPathChange(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                onOpenDirectPath();
              }
            }}
          />
          <Button
            isDisabled={directPath.trim().length === 0 || loading}
            size="sm"
            variant="ghost"
            onPress={onOpenDirectPath}
          >
            Open path
          </Button>
        </div>
      </aside>

      <section className="directory-browser__main" aria-label="Directory browser">
        <div className="directory-browser__toolbar">
          {page === undefined ? (
            <div className="directory-browser__welcome">
              <FolderOpen size={22} aria-hidden="true" />
              Choose a location to browse its directories.
            </div>
          ) : (
            <>
              <nav
                aria-label="Selected folder breadcrumbs"
                className="directory-breadcrumbs"
                tabIndex={0}
              >
                {page.breadcrumbs.map((breadcrumb, index) => (
                  <span key={breadcrumb.browseToken}>
                    {index > 0 ? <ChevronRight size={12} aria-hidden="true" /> : null}
                    <button
                      type="button"
                      disabled={loading || index === page.breadcrumbs.length - 1}
                      onClick={() => onNavigate(breadcrumb.browseToken)}
                    >
                      {breadcrumb.label}
                    </button>
                  </span>
                ))}
              </nav>
              <label className="directory-filter">
                <Search size={15} aria-hidden="true" />
                <span className="sr-only">Filter directories</span>
                <input
                  autoFocus
                  aria-label="Filter directories"
                  id="directory-filter"
                  maxLength={256}
                  name="directory-filter"
                  placeholder="Filter this folder"
                  value={filter}
                  onChange={(event) => onFilterChange(event.currentTarget.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      onApplyFilter();
                    }
                  }}
                />
                <Button isDisabled={loading} size="sm" variant="ghost" onPress={onApplyFilter}>
                  Apply
                </Button>
              </label>
              <DirectoryList
                directories={page.directories}
                loading={loading}
                onOpenChild={onOpenChild}
              />
              {page.nextCursor === null ? null : (
                <Button
                  className="directory-load-more"
                  isDisabled={loading}
                  size="sm"
                  variant="ghost"
                  onPress={onLoadMore}
                >
                  Load more folders
                </Button>
              )}
            </>
          )}
        </div>

        <ModeSelector selected={selectedMode} onChange={onModeChange} />
      </section>
    </div>
  );
}

function DirectoryList({
  directories,
  loading,
  onOpenChild,
}: {
  directories: FilesystemDirectory[];
  loading: boolean;
  onOpenChild: (name: string) => void;
}) {
  const rowReferences = useRef<(HTMLButtonElement | null)[]>([]);

  function moveFocus(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") {
      return;
    }
    event.preventDefault();
    const delta = event.key === "ArrowDown" ? 1 : -1;
    const next = Math.min(Math.max(index + delta, 0), directories.length - 1);
    rowReferences.current[next]?.focus();
  }

  if (directories.length === 0) {
    return (
      <div className="directory-empty" role="status">
        This folder has no visible child directories.
      </div>
    );
  }
  return (
    <ul className="directory-list" aria-label="Child directories" aria-busy={loading}>
      {directories.map((directory, index) => (
        <li key={directory.name}>
          <button
            ref={(element) => {
              rowReferences.current[index] = element;
            }}
            type="button"
            disabled={!directory.readable || !directory.selectable || loading}
            onClick={() => onOpenChild(directory.name)}
            onKeyDown={(event) => moveFocus(event, index)}
          >
            <Folder size={16} aria-hidden="true" />
            <span>{directory.name}</span>
            <ChevronRight size={14} aria-hidden="true" />
          </button>
        </li>
      ))}
    </ul>
  );
}

function ModeSelector({
  selected,
  onChange,
}: {
  selected: IndexMode;
  onChange: (mode: IndexMode) => void;
}) {
  const modes: { mode: IndexMode; title: string; detail: string }[] = [
    {
      mode: "auto",
      title: "Auto",
      detail: "Publish structural results first and refine semantics separately when available.",
    },
    {
      mode: "structural",
      title: "Structural",
      detail: "Use audited in-process analyzers without optional semantic refinement.",
    },
    {
      mode: "deep",
      title: "Deep",
      detail: "Request the strongest isolated whole-project analysis supported by the daemon.",
    },
  ];
  return (
    <fieldset className="index-mode-selector">
      <legend>Analysis mode</legend>
      {modes.map((choice) => (
        <label key={choice.mode} className={selected === choice.mode ? "is-selected" : undefined}>
          <input
            type="radio"
            name="index-mode"
            checked={selected === choice.mode}
            value={choice.mode}
            onChange={() => onChange(choice.mode)}
          />
          <span>
            <strong>{choice.title}</strong>
            <small>{choice.detail}</small>
          </span>
        </label>
      ))}
    </fieldset>
  );
}

function PreflightReview({ preflight }: { preflight: IndexPreflight }) {
  const ready = preflight.selectable && preflight.daemonAcceptingOperations;
  return (
    <section className="preflight-review" aria-labelledby="preflight-heading">
      <div className={ready ? "preflight-review__status is-ready" : "preflight-review__status"}>
        {ready ? (
          <ShieldCheck size={22} aria-hidden="true" />
        ) : (
          <TriangleAlert size={22} aria-hidden="true" />
        )}
        <div>
          <h3 id="preflight-heading">{ready ? "Ready to index" : "Index admission unavailable"}</h3>
          <p>{preflight.normalizedDisplayLabel}</p>
        </div>
      </div>
      <dl className="preflight-facts">
        <div>
          <dt>Mode</dt>
          <dd>{preflight.selectedMode}</dd>
        </div>
        <div>
          <dt>Daemon admission</dt>
          <dd>{preflight.daemonAcceptingOperations ? "accepting operations" : "paused"}</dd>
        </div>
        <div>
          <dt>Adapter isolation</dt>
          <dd>{preflight.adapterIsolation.replaceAll("_", " ")}</dd>
        </div>
        <div>
          <dt>Capability lifetime</dt>
          <dd>{preflight.rootCapabilityExpiresInSeconds} seconds</dd>
        </div>
      </dl>
      {preflight.warnings.length === 0 ? null : (
        <div className="preflight-warnings">
          <Info size={16} aria-hidden="true" />
          <ul>
            {preflight.warnings.map((warning) => (
              <li key={warning}>{warning.replaceAll("_", " ")}</li>
            ))}
          </ul>
        </div>
      )}
      <p className="detached-operation-note">
        The index continues in the daemon if this tab closes. Cancellation is always an explicit
        action from the operation row.
      </p>
    </section>
  );
}

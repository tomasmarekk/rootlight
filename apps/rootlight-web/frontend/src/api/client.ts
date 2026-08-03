// Enforces bounded same-origin API reads and keeps CSRF credentials in memory.

import {
  parseFilesystemBrowsePage,
  parseFilesystemRoots,
  parseHealth,
  parseIndexPreflight,
  parseOpenFilesystemPath,
  parseOperationCancel,
  parseProjectCatalogPage,
  parseProjectDetail,
  parseProjectIndexAdmission,
  parseRepositoryOperation,
  parseSession,
  type FilesystemBrowsePage,
  type FilesystemRoots,
  type Health,
  type IndexMode,
  type IndexPreflight,
  type OpenFilesystemPath,
  type OperationCancel,
  type ProjectCatalogPage,
  type ProjectDetail,
  type ProjectIndexAdmission,
  type ProjectLifecycleFilter,
  type RepositoryOperation,
  type Session,
} from "./contracts";
import {
  parseBrowserGraphPage,
  parseBrowserGraphRelease,
  type BrowserGraphPage,
  type BrowserGraphRelease,
  type GraphProjectionOpenRequest,
} from "../features/graph/model/graph-contracts";

const maximumJsonBytes = 1024 * 1024;
const maximumErrorBytes = 16 * 1024;
const bootstrapPattern = /^[A-Za-z0-9_-]{43}$/u;
const publicErrorPattern = /^[a-z][a-z0-9_]{0,63}$/u;

let csrfToken: string | undefined;
let initialization: Promise<Session> | undefined;

export class ApiError extends Error {
  public readonly status: number;
  public readonly code: string;

  public constructor(status: number, code: string) {
    super(code);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

export function initializeSession(): Promise<Session> {
  initialization ??= initializeSessionOnce();
  return initialization;
}

export async function fetchHealth(signal?: AbortSignal): Promise<Health> {
  return parseHealth(await requestJson("/api/v1/health", { signal }));
}

export type ProjectCatalogRequest = {
  pageSize: number;
  query?: string;
  states?: ProjectLifecycleFilter[];
  snapshot?: string;
  after?: string;
  sortVersion?: number;
};

export async function fetchProjects(
  catalog: ProjectCatalogRequest,
  signal?: AbortSignal,
): Promise<ProjectCatalogPage> {
  const parameters = new URLSearchParams();
  parameters.set("page_size", String(catalog.pageSize));
  if (catalog.query !== undefined && catalog.query.length > 0) {
    parameters.set("query", catalog.query);
  }
  for (const state of catalog.states ?? []) {
    parameters.append("state", state);
  }
  if (catalog.snapshot !== undefined) {
    parameters.set("snapshot", catalog.snapshot);
  }
  if (catalog.after !== undefined) {
    parameters.set("after", catalog.after);
  }
  if (catalog.sortVersion !== undefined) {
    parameters.set("sort_version", String(catalog.sortVersion));
  }
  return parseProjectCatalogPage(
    await requestJson(`/api/v1/projects?${parameters.toString()}`, { signal }),
  );
}

export async function fetchProjectDetail(
  repositoryId: string,
  generation: string,
  signal?: AbortSignal,
): Promise<ProjectDetail> {
  const parameters = new URLSearchParams({
    generation,
    coverage_detail: "language",
    include_operations: "true",
    require_freshness: "none",
  });
  return parseProjectDetail(
    await requestJson(
      `/api/v1/projects/${encodeURIComponent(repositoryId)}?${parameters.toString()}`,
      { signal },
    ),
    repositoryId,
    generation,
  );
}

export async function fetchFilesystemRoots(signal?: AbortSignal): Promise<FilesystemRoots> {
  return parseFilesystemRoots(await requestJson("/api/v1/filesystem/roots", { signal }));
}

export async function openFilesystemPath(
  path: string,
  signal?: AbortSignal,
): Promise<OpenFilesystemPath> {
  return parseOpenFilesystemPath(
    await mutationJson("/api/v1/filesystem/open-path", { path }, signal),
  );
}

export type FilesystemBrowseRequest = {
  browseToken: string;
  action: { type: "current" } | { type: "parent" } | { type: "child"; name: string };
  pageSize: number;
  cursor?: string;
  filter?: string;
};

export async function browseFilesystem(
  browse: FilesystemBrowseRequest,
  signal?: AbortSignal,
): Promise<FilesystemBrowsePage> {
  return parseFilesystemBrowsePage(
    await mutationJson(
      "/api/v1/filesystem/browse",
      {
        browseToken: browse.browseToken,
        action: browse.action,
        pageSize: browse.pageSize,
        cursor: browse.cursor,
        filter: browse.filter,
      },
      signal,
    ),
  );
}

export async function preflightFilesystemIndex(
  browseToken: string,
  mode: IndexMode,
  signal?: AbortSignal,
): Promise<IndexPreflight> {
  return parseIndexPreflight(
    await mutationJson("/api/v1/filesystem/preflight-index", { browseToken, mode }, signal),
  );
}

export type ProjectIndexRequest = {
  rootCapability: string;
  mode: IndexMode;
  clientRequestId: string;
};

export async function submitProjectIndex(
  index: ProjectIndexRequest,
  signal?: AbortSignal,
): Promise<ProjectIndexAdmission> {
  return parseProjectIndexAdmission(
    await mutationJson(
      "/api/v1/projects/index",
      {
        rootCapability: index.rootCapability,
        mode: index.mode,
        detached: true,
        clientRequestId: index.clientRequestId,
      },
      signal,
    ),
  );
}

export type OperationStatusRequest = {
  waitMs?: number;
  afterRevision?: string;
};

export async function fetchIndexOperation(
  operationId: string,
  status: OperationStatusRequest = {},
  signal?: AbortSignal,
): Promise<RepositoryOperation> {
  const parameters = new URLSearchParams();
  if (status.waitMs !== undefined) {
    parameters.set("wait_ms", String(status.waitMs));
  }
  if (status.afterRevision !== undefined) {
    parameters.set("after_revision", status.afterRevision);
  }
  const query = parameters.size === 0 ? "" : `?${parameters.toString()}`;
  return parseRepositoryOperation(
    await requestJson(`/api/v1/operations/${encodeURIComponent(operationId)}${query}`, { signal }),
    operationId,
  );
}

export async function cancelIndexOperation(
  operationId: string,
  signal?: AbortSignal,
): Promise<OperationCancel> {
  return parseOperationCancel(
    await mutationJson(`/api/v1/operations/${encodeURIComponent(operationId)}/cancel`, {}, signal),
    operationId,
  );
}

export function createClientRequestId(): string {
  const entropy = new Uint8Array(24);
  crypto.getRandomValues(entropy);
  return `idx_${Array.from(entropy, (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

export async function openGraphProjection(
  request: GraphProjectionOpenRequest,
  signal?: AbortSignal,
): Promise<BrowserGraphPage> {
  return parseBrowserGraphPage(
    await mutationJson("/api/v1/graph/projections", request, signal),
    request.repositoryId,
    request.generationId,
  );
}

export async function fetchNextGraphPage(
  projectionToken: string,
  repositoryId: string,
  generationId: string,
  signal?: AbortSignal,
): Promise<BrowserGraphPage> {
  return parseBrowserGraphPage(
    await mutationJson(
      `/api/v1/graph/projections/${encodeURIComponent(projectionToken)}/next`,
      {},
      signal,
    ),
    repositoryId,
    generationId,
    projectionToken,
  );
}

export async function releaseGraphProjection(
  projectionToken: string,
  signal?: AbortSignal,
): Promise<BrowserGraphRelease> {
  return parseBrowserGraphRelease(
    await mutationJson(
      `/api/v1/graph/projections/${encodeURIComponent(projectionToken)}`,
      {},
      signal,
      "DELETE",
    ),
  );
}

export async function logout(): Promise<void> {
  await request("/api/v1/session", {
    method: "DELETE",
    headers: csrfToken === undefined ? undefined : { "x-rootlight-csrf": csrfToken },
  });
  csrfToken = undefined;
  initialization = undefined;
}

async function mutationJson(
  path: string,
  body: unknown,
  signal?: AbortSignal,
  method: "DELETE" | "POST" = "POST",
): Promise<unknown> {
  if (csrfToken === undefined) {
    throw new ApiError(401, "session_required");
  }
  return requestJson(path, {
    method,
    headers: {
      "content-type": "application/json",
      "x-rootlight-csrf": csrfToken,
    },
    body: JSON.stringify(body),
    signal,
  });
}

async function initializeSessionOnce(): Promise<Session> {
  const bootstrap = takeBootstrapSecret();
  const session =
    bootstrap === undefined
      ? parseSession(await requestJson("/api/v1/session"))
      : parseSession(
          await requestJson("/api/v1/session/bootstrap", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ secret: bootstrap }),
          }),
        );
  csrfToken = session.csrfToken;
  return session;
}

function takeBootstrapSecret(): string | undefined {
  const fragment = window.location.hash;
  if (!fragment.startsWith("#bootstrap=")) {
    return undefined;
  }
  window.history.replaceState(
    window.history.state,
    "",
    `${window.location.pathname}${window.location.search}`,
  );
  const secret = fragment.slice("#bootstrap=".length);
  if (!bootstrapPattern.test(secret)) {
    throw new ApiError(401, "invalid_bootstrap");
  }
  return secret;
}

async function requestJson(path: string, init?: RequestInit): Promise<unknown> {
  const response = await request(path, init);
  const contentLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(contentLength) && contentLength > maximumJsonBytes) {
    throw new ApiError(502, "response_too_large");
  }
  const body = await response.arrayBuffer();
  if (body.byteLength > maximumJsonBytes) {
    throw new ApiError(502, "response_too_large");
  }
  try {
    return JSON.parse(new TextDecoder().decode(body)) as unknown;
  } catch {
    throw new ApiError(502, "invalid_response");
  }
}

async function request(path: string, init?: RequestInit): Promise<Response> {
  const response = await fetch(path, {
    ...init,
    credentials: "same-origin",
    redirect: "error",
    cache: "no-store",
  });
  if (!response.ok) {
    throw new ApiError(response.status, await readErrorCode(response));
  }
  return response;
}

async function readErrorCode(response: Response): Promise<string> {
  const fallback = response.status === 401 ? "session_required" : "request_failed";
  const contentLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(contentLength) && contentLength > maximumErrorBytes) {
    return fallback;
  }
  try {
    const body = await response.arrayBuffer();
    if (body.byteLength > maximumErrorBytes) {
      return fallback;
    }
    const parsed = JSON.parse(new TextDecoder().decode(body)) as unknown;
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return fallback;
    }
    const error = (parsed as Record<string, unknown>).error;
    if (typeof error !== "object" || error === null || Array.isArray(error)) {
      return fallback;
    }
    const code = (error as Record<string, unknown>).code;
    return typeof code === "string" && publicErrorPattern.test(code) ? code : fallback;
  } catch {
    return fallback;
  }
}

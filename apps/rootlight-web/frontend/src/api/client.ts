// Enforces bounded same-origin API reads and keeps CSRF credentials in memory.

import { parseHealth, parseSession, type Health, type Session } from "./contracts";

const maximumJsonBytes = 1024 * 1024;
const bootstrapPattern = /^[A-Za-z0-9_-]{43}$/u;

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

export async function logout(): Promise<void> {
  await request("/api/v1/session", {
    method: "DELETE",
    headers: csrfToken === undefined ? undefined : { "x-rootlight-csrf": csrfToken },
  });
  csrfToken = undefined;
  initialization = undefined;
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
    throw new ApiError(
      response.status,
      response.status === 401 ? "session_required" : "request_failed",
    );
  }
  return response;
}

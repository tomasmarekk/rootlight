// Serves the production bundle under the same strict browser policy as rootlight-web.
// The acceptance suite therefore observes CSP regressions that Vite preview would hide.

import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, resolve, sep } from "node:path";
import process from "node:process";
import { URL } from "node:url";

const host = "127.0.0.1";
const port = Number.parseInt(process.env.ROOTLIGHT_WEB_E2E_PORT ?? "4173", 10);
const distRoot = resolve(import.meta.dirname, "../../dist");
const indexPath = resolve(distRoot, "index.html");
const contentSecurityPolicy =
  "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; " +
  "connect-src 'self'; font-src 'self'; worker-src 'self'; base-uri 'none'; " +
  "form-action 'none'; frame-ancestors 'none'; object-src 'none'";

if (!existsSync(indexPath)) {
  throw new Error("production dist is missing; run the build before the browser suite");
}

const server = createServer((request, response) => {
  const requestUrl = new URL(request.url ?? "/", `http://${host}:${String(port)}`);
  let pathname;
  try {
    pathname = decodeURIComponent(requestUrl.pathname);
  } catch {
    respond(response, 400, "text/plain; charset=utf-8", "Invalid URL encoding");
    return;
  }

  const requestedPath = resolve(distRoot, `.${pathname}`);
  if (requestedPath !== distRoot && !requestedPath.startsWith(`${distRoot}${sep}`)) {
    respond(response, 404, "text/plain; charset=utf-8", "Not found");
    return;
  }

  const assetPath =
    requestedPath !== distRoot && existsSync(requestedPath) && statSync(requestedPath).isFile()
      ? requestedPath
      : extname(pathname) === ""
        ? indexPath
        : null;
  if (assetPath === null) {
    respond(response, 404, "text/plain; charset=utf-8", "Not found");
    return;
  }

  response.writeHead(200, securityHeaders(contentType(assetPath)));
  if (request.method === "HEAD") {
    response.end();
    return;
  }
  createReadStream(assetPath).pipe(response);
});

server.listen(port, host, () => {
  process.stdout.write(`Rootlight browser fixture listening on http://${host}:${String(port)}\n`);
});

function securityHeaders(type) {
  return {
    "Cache-Control": "no-store",
    "Content-Security-Policy": contentSecurityPolicy,
    "Content-Type": type,
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Resource-Policy": "same-origin",
    "Permissions-Policy":
      "bluetooth=(), camera=(), clipboard-read=(), display-capture=(), geolocation=(), " +
      "microphone=(), payment=(), serial=(), usb=()",
    "Referrer-Policy": "no-referrer",
    "X-Content-Type-Options": "nosniff",
    "X-Frame-Options": "DENY",
  };
}

function contentType(path) {
  switch (extname(path)) {
    case ".css":
      return "text/css; charset=utf-8";
    case ".html":
      return "text/html; charset=utf-8";
    case ".js":
      return "text/javascript; charset=utf-8";
    case ".json":
      return "application/json; charset=utf-8";
    case ".svg":
      return "image/svg+xml";
    default:
      return "application/octet-stream";
  }
}

function respond(response, status, type, body) {
  response.writeHead(status, securityHeaders(type));
  response.end(body);
}

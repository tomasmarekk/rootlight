// Verifies the immutable browser asset set before native package construction.
//
// The report binds the target-neutral assets, npm lockfile, exact toolchain,
// and generated notice inventory without exposing checkout-specific paths.

import { createHash } from "node:crypto";
import { lstat, readdir, readFile, writeFile } from "node:fs/promises";
import { relative, resolve, sep } from "node:path";

const options = parseOptions(process.argv.slice(2));
if (process.version !== "v24.11.1" || options.npmVersion !== "11.6.2") {
  throw new Error("web release assets require Node 24.11.1 and npm 11.6.2");
}
if (!/^[0-9a-f]{40}$/u.test(options.sourceRevision)) {
  throw new Error("source revision is not a canonical Git SHA-1");
}
if (
  !/^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-alpha\.(?:0|[1-9][0-9]*))?$/u.test(
    options.version,
  )
) {
  throw new Error("release version is not a supported canonical SemVer");
}

const frontendRoot = resolve(import.meta.dirname, "..");
const assetRoot = resolve(options.assetsDir);
const rootMetadata = await lstat(assetRoot);
if (!rootMetadata.isDirectory() || rootMetadata.isSymbolicLink()) {
  throw new Error("web asset root must be a non-link directory");
}
const manifestPath = resolve(assetRoot, "asset-manifest.json");
const manifestBytes = await readRegular(manifestPath, 1024 * 1024);
const manifest = JSON.parse(manifestBytes.toString("utf8"));
if (
  manifest.schema_version !== 1 ||
  !Array.isArray(manifest.assets) ||
  manifest.assets.length === 0 ||
  manifest.assets.length > 1024
) {
  throw new Error("web asset manifest identity or count is invalid");
}

const files = await collectFiles(assetRoot);
files.delete("asset-manifest.json");
const declared = new Set();
let previous = "";
let totalBytes = 0;
for (const asset of manifest.assets) {
  if (
    typeof asset.path !== "string" ||
    typeof asset.bytes !== "number" ||
    !Number.isSafeInteger(asset.bytes) ||
    asset.bytes <= 0 ||
    asset.bytes > 16 * 1024 * 1024 ||
    typeof asset.sha256 !== "string" ||
    !/^[0-9a-f]{64}$/u.test(asset.sha256) ||
    asset.path <= previous ||
    !validAssetPath(asset.path) ||
    declared.has(asset.path)
  ) {
    throw new Error("web asset manifest record is invalid");
  }
  previous = asset.path;
  declared.add(asset.path);
  const bytes = files.get(asset.path);
  if (bytes === undefined || bytes.length !== asset.bytes || sha256(bytes) !== asset.sha256) {
    throw new Error("web asset content differs from its manifest");
  }
  totalBytes += asset.bytes;
  if (totalBytes > 64 * 1024 * 1024) {
    throw new Error("web asset inventory exceeds its total-byte limit");
  }
  verifyNoExternalRuntime(asset.path, bytes);
}
if (
  files.size !== declared.size ||
  [...files.keys()].some((path) => !declared.has(path)) ||
  !declared.has("index.html")
) {
  throw new Error("web asset tree differs from its manifest");
}
verifyEntrypoint(files.get("index.html"), declared);

const lockPath = resolve(frontendRoot, "package-lock.json");
const packagePath = resolve(frontendRoot, "package.json");
const lockBytes = await readRegular(lockPath, 16 * 1024 * 1024);
const packageBytes = await readRegular(packagePath, 1024 * 1024);
const lock = JSON.parse(lockBytes.toString("utf8"));
const ignoredOptionalInstallScripts = [];
for (const [path, dependency] of Object.entries(lock.packages ?? {})) {
  if (path === "") {
    continue;
  }
  if (dependency.hasInstallScript === true) {
    // fsevents is a Darwin-only optional watcher dependency. It remains locked
    // for reproducibility, while every release install uses --ignore-scripts.
    if (
      dependency.dev !== true ||
      dependency.optional !== true ||
      !path.endsWith("/fsevents") ||
      !Array.isArray(dependency.os) ||
      dependency.os.length !== 1 ||
      dependency.os[0] !== "darwin"
    ) {
      throw new Error("npm dependency lifecycle script is not explicitly allowlisted");
    }
    ignoredOptionalInstallScripts.push(path);
  }
  if (
    typeof dependency.resolved === "string" &&
    !dependency.resolved.startsWith("https://registry.npmjs.org/")
  ) {
    throw new Error("npm dependency does not come from the approved registry");
  }
}

const noticeBytes = await readRegular(resolve(options.notices), 8 * 1024 * 1024);
if (noticeBytes.length === 0) {
  throw new Error("web third-party notice inventory is empty");
}
const report = {
  schema: "rootlight.web-release-assets/1",
  version: options.version,
  source_revision: options.sourceRevision,
  node_version: process.version.slice(1),
  npm_version: options.npmVersion,
  package_json_sha256: sha256(packageBytes),
  package_lock_sha256: sha256(lockBytes),
  asset_manifest_sha256: sha256(manifestBytes),
  notice_sha256: sha256(noticeBytes),
  ignored_optional_install_scripts: ignoredOptionalInstallScripts,
  asset_count: manifest.assets.length,
  asset_bytes: totalBytes,
};
await writeFile(resolve(options.output), `${JSON.stringify(report, null, 2)}\n`, {
  encoding: "utf8",
  flag: "wx",
});

async function collectFiles(root) {
  const pending = [root];
  const files = new Map();
  while (pending.length > 0) {
    const directory = pending.pop();
    const entries = (await readdir(directory, { withFileTypes: true })).sort((left, right) =>
      compareText(left.name, right.name),
    );
    for (const entry of entries.toReversed()) {
      const path = resolve(directory, entry.name);
      const metadata = await lstat(path);
      if (entry.isSymbolicLink() || metadata.isSymbolicLink()) {
        throw new Error("web asset tree contains a link");
      }
      if (entry.isDirectory() && metadata.isDirectory()) {
        pending.push(path);
      } else if (entry.isFile() && metadata.isFile()) {
        const name = relative(root, path).split(sep).join("/");
        if (files.size >= 1025 || files.has(name)) {
          throw new Error("web asset tree file count or identity is invalid");
        }
        files.set(name, await readRegular(path, 16 * 1024 * 1024));
      } else {
        throw new Error("web asset tree contains an unsupported file type");
      }
    }
  }
  return files;
}

async function readRegular(path, maximum) {
  const metadata = await lstat(path);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > maximum) {
    throw new Error("release input is not a bounded regular file");
  }
  const bytes = await readFile(path);
  if (bytes.length !== metadata.size) {
    throw new Error("release input changed while it was read");
  }
  return bytes;
}

function validAssetPath(path) {
  if (
    path.length === 0 ||
    path.length > 512 ||
    path.startsWith("/") ||
    path.endsWith("/") ||
    path.includes("\\") ||
    path.endsWith(".map") ||
    path.split("/").some((component) => component === "" || component === "." || component === "..")
  ) {
    return false;
  }
  if (path === "index.html") {
    return true;
  }
  const name = path.startsWith("assets/") ? path.slice("assets/".length) : "";
  if (name.length === 0 || name.includes("/")) {
    return false;
  }
  const stem = name.includes(".") ? name.slice(0, name.lastIndexOf(".")) : name;
  const hash = stem.includes("-") ? stem.slice(stem.lastIndexOf("-") + 1) : stem;
  return hash.length >= 8 && /^[0-9A-Za-z]+$/u.test(hash);
}

function verifyEntrypoint(bytes, assets) {
  if (bytes === undefined) {
    throw new Error("web entrypoint is missing");
  }
  const html = bytes.toString("utf8");
  for (const match of html.matchAll(/\b(?:src|href)="([^"]+)"/gu)) {
    const reference = match[1];
    if (/^(?:[a-z]+:)?\/\//iu.test(reference)) {
      throw new Error("web entrypoint references an external URL");
    }
    const path = reference.startsWith("/") ? reference.slice(1) : reference;
    if (!assets.has(path)) {
      throw new Error("web entrypoint references an asset outside its manifest");
    }
  }
}

function verifyNoExternalRuntime(path, bytes) {
  if (!/\.(?:css|html|js)$/u.test(path)) {
    return;
  }
  const text = bytes.toString("utf8");
  if (
    /sourceMappingURL\s*=/u.test(text) ||
    /\b(?:fetch|WebSocket|EventSource|Worker)\(\s*["'`]https?:\/\//u.test(text) ||
    /\bimport\s*\(\s*["'`]https?:\/\//u.test(text) ||
    /\burl\(\s*["']?https?:\/\//u.test(text)
  ) {
    throw new Error("web asset contains an external runtime reference");
  }
}

function parseOptions(arguments_) {
  const values = {};
  for (let index = 0; index < arguments_.length; index += 2) {
    const flag = arguments_[index];
    const value = arguments_[index + 1];
    if (
      ![
        "--assets-dir",
        "--notices",
        "--npm-version",
        "--output",
        "--source-revision",
        "--version",
      ].includes(flag) ||
      value === undefined ||
      Object.hasOwn(values, flag)
    ) {
      throw new Error("web release verifier arguments are invalid");
    }
    values[flag] = value;
  }
  for (const flag of [
    "--assets-dir",
    "--notices",
    "--npm-version",
    "--output",
    "--source-revision",
    "--version",
  ]) {
    if (!Object.hasOwn(values, flag)) {
      throw new Error(`web release verifier requires ${flag}`);
    }
  }
  return {
    assetsDir: values["--assets-dir"],
    notices: values["--notices"],
    npmVersion: values["--npm-version"],
    output: values["--output"],
    sourceRevision: values["--source-revision"],
    version: values["--version"],
  };
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

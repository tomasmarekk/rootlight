// Verifies the clean npm installation against both lockfile inventories.
//
// npm's tree reporter can reject a valid optional fallback when that fallback
// declares peers supplied only by its parent package. This check instead binds
// every installed package to npm's hidden installation lock, then binds that
// snapshot back to the repository lockfile without weakening required-package
// coverage.

import { lstat, readdir, readFile } from "node:fs/promises";
import { resolve } from "node:path";

if (
  process.version !== "v24.11.1" ||
  !process.env.npm_config_user_agent?.startsWith("npm/11.6.2 ")
) {
  throw new Error("web dependencies require Node 24.11.1 and npm 11.6.2");
}

const frontendRoot = resolve(import.meta.dirname, "..");
const packageDocument = await readJson(resolve(frontendRoot, "package.json"), 1024 * 1024);
const repositoryLock = await readJson(resolve(frontendRoot, "package-lock.json"), 16 * 1024 * 1024);
const installationLock = await readJson(
  resolve(frontendRoot, "node_modules/.package-lock.json"),
  16 * 1024 * 1024,
);

if (
  repositoryLock.lockfileVersion !== 3 ||
  installationLock.lockfileVersion !== 3 ||
  repositoryLock.name !== packageDocument.name ||
  repositoryLock.version !== packageDocument.version ||
  !isRecord(repositoryLock.packages) ||
  !isRecord(installationLock.packages)
) {
  throw new Error("npm lockfile identity is invalid");
}
const rootRecord = repositoryLock.packages[""];
if (
  !isRecord(rootRecord) ||
  rootRecord.name !== packageDocument.name ||
  rootRecord.version !== packageDocument.version ||
  !sameJson(rootRecord.dependencies, packageDocument.dependencies) ||
  !sameJson(rootRecord.devDependencies, packageDocument.devDependencies) ||
  !sameJson(rootRecord.engines, packageDocument.engines)
) {
  throw new Error("package.json differs from the repository lockfile root");
}

const repositoryPackages = Object.entries(repositoryLock.packages).filter(([path]) => path !== "");
const installationPackages = Object.entries(installationLock.packages);
if (
  repositoryPackages.length === 0 ||
  repositoryPackages.length > 2048 ||
  installationPackages.length === 0 ||
  installationPackages.length > repositoryPackages.length
) {
  throw new Error("npm package inventory count is invalid");
}

const installedOnDisk = await collectInstalledPackages(resolve(frontendRoot, "node_modules"));
if (
  installedOnDisk.size !== installationPackages.length ||
  installationPackages.some(([path]) => !installedOnDisk.has(path))
) {
  throw new Error("installed package tree differs from npm's installation lock");
}

for (const [path, installedRecord] of installationPackages) {
  const repositoryRecord = repositoryLock.packages[path];
  const packageRecord = installedOnDisk.get(path);
  if (
    !validPackagePath(path) ||
    !isRecord(installedRecord) ||
    !isRecord(repositoryRecord) ||
    !isRecord(packageRecord) ||
    packageRecord.name !== packageNameFromPath(path) ||
    (packageRecord.name !== installedRecord.name?.toString().trim() &&
      installedRecord.name !== undefined) ||
    packageRecord.version !== installedRecord.version ||
    typeof packageRecord.license !== "string" ||
    packageRecord.license !== repositoryRecord.license ||
    installedRecord.version !== repositoryRecord.version ||
    installedRecord.resolved !== repositoryRecord.resolved ||
    installedRecord.integrity !== repositoryRecord.integrity
  ) {
    throw new Error(`installed npm package identity differs: ${path}`);
  }
}

for (const [path, dependency] of repositoryPackages) {
  if (!isRecord(dependency) || !validPackagePath(path)) {
    throw new Error("repository npm package record is invalid");
  }
  if (dependency.optional !== true && !Object.hasOwn(installationLock.packages, path)) {
    throw new Error(`required npm package is absent from the installation: ${path}`);
  }
}

async function collectInstalledPackages(nodeModulesRoot) {
  const rootMetadata = await lstat(nodeModulesRoot);
  if (!rootMetadata.isDirectory() || rootMetadata.isSymbolicLink()) {
    throw new Error("node_modules must be a non-link directory");
  }

  const packages = new Map();
  const pending = [{ directory: nodeModulesRoot, prefix: "node_modules" }];
  while (pending.length > 0) {
    const { directory, prefix } = pending.pop();
    const entries = (await readdir(directory, { withFileTypes: true })).sort((left, right) =>
      compareText(left.name, right.name),
    );
    for (const entry of entries.toReversed()) {
      if (
        entry.name === ".bin" ||
        (prefix === "node_modules" && entry.name === ".package-lock.json")
      ) {
        continue;
      }
      const entryPath = resolve(directory, entry.name);
      const metadata = await lstat(entryPath);
      if (!entry.isDirectory() || !metadata.isDirectory() || metadata.isSymbolicLink()) {
        throw new Error("node_modules contains an unsupported top-level entry");
      }
      if (entry.name.startsWith("@")) {
        const scopedEntries = (await readdir(entryPath, { withFileTypes: true })).sort(
          (left, right) => compareText(left.name, right.name),
        );
        for (const scopedEntry of scopedEntries.toReversed()) {
          const packagePath = resolve(entryPath, scopedEntry.name);
          await addPackage(
            packages,
            pending,
            packagePath,
            `${prefix}/${entry.name}/${scopedEntry.name}`,
          );
        }
      } else {
        await addPackage(packages, pending, entryPath, `${prefix}/${entry.name}`);
      }
    }
  }
  return packages;
}

async function addPackage(packages, pending, directory, packagePath) {
  const metadata = await lstat(directory);
  if (!metadata.isDirectory() || metadata.isSymbolicLink() || packages.has(packagePath)) {
    throw new Error("installed npm package directory is invalid");
  }
  packages.set(packagePath, await readJson(resolve(directory, "package.json"), 1024 * 1024));
  const nestedNodeModules = resolve(directory, "node_modules");
  try {
    const nestedMetadata = await lstat(nestedNodeModules);
    if (!nestedMetadata.isDirectory() || nestedMetadata.isSymbolicLink()) {
      throw new Error("nested node_modules is not a non-link directory");
    }
    pending.push({
      directory: nestedNodeModules,
      prefix: `${packagePath}/node_modules`,
    });
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw error;
    }
  }
}

async function readJson(path, maximum) {
  const metadata = await lstat(path);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > maximum) {
    throw new Error("npm metadata is not a bounded regular file");
  }
  const bytes = await readFile(path);
  if (bytes.length !== metadata.size) {
    throw new Error("npm metadata changed while it was read");
  }
  return JSON.parse(bytes.toString("utf8"));
}

function validPackagePath(path) {
  if (
    typeof path !== "string" ||
    path.length === 0 ||
    path.length > 1024 ||
    path.includes("\\") ||
    path.startsWith("/") ||
    path.endsWith("/") ||
    path.split("/").some((component) => component === "" || component === "." || component === "..")
  ) {
    return false;
  }
  const components = path.split("/");
  let index = 0;
  while (index < components.length) {
    if (components[index] !== "node_modules" || index + 1 >= components.length) {
      return false;
    }
    index += components[index + 1].startsWith("@") ? 3 : 2;
  }
  return index === components.length;
}

function packageNameFromPath(path) {
  const suffix = path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length);
  return suffix;
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function sameJson(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

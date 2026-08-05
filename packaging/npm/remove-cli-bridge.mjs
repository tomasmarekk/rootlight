#!/usr/bin/env node
// rootlight npm cli bridge

import {
  existsSync,
  lstatSync,
  readFileSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { dirname, isAbsolute, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const cliBridgeSchema = "rootlight.npm-cli-bridge/1";
const cliBridgeMarker = "rootlight npm cli bridge";
const cliBridgeManifest = ".rootlight-cli-bridge.json";
const cliBridgeCleanupRuntime = ".rootlight-cli-cleanup.mjs";
const maximumBridgeFileBytes = 64 * 1024;
const maximumRemovalAttempts = 40;
const removalRetryDelayMilliseconds = 50;
const initialRemovalDelayMilliseconds = 250;
const publicExecutables = ["rootlight", "rootlight-mcp"];

if (process.platform !== "win32" || process.argv.length !== 5) {
  fail("rootlight: invalid deferred CLI cleanup request");
}

const expectedPackageRoot = process.argv[2];
const expectedInstallPrefix = process.argv[3];
const expectedBinDirectory = process.argv[4];
const binDirectory = dirname(fileURLToPath(import.meta.url));
if (
  !isAbsolute(expectedPackageRoot) ||
  !isAbsolute(expectedInstallPrefix) ||
  !isAbsolute(expectedBinDirectory) ||
  !samePath(binDirectory, expectedBinDirectory)
) {
  fail("rootlight: invalid deferred CLI cleanup paths");
}

await delay(initialRemovalDelayMilliseconds);

const manifestPath = join(binDirectory, cliBridgeManifest);
const manifest = readBridgeManifest(manifestPath);
if (
  !samePath(manifest.packageRoot, expectedPackageRoot) ||
  !samePath(manifest.installPrefix, expectedInstallPrefix)
) {
  fail("rootlight: deferred CLI cleanup ownership differs");
}

const ownedFiles = bridgeFileNames();
for (const name of ownedFiles) {
  const path = join(binDirectory, name);
  if (existsSync(path)) {
    validateOwnedBridgeFile(path);
  }
}
for (const name of ownedFiles) {
  if (name !== cliBridgeCleanupRuntime) {
    await removeOwnedPath(join(binDirectory, name));
  }
}
await removeOwnedPath(manifestPath);
await removeOwnedPath(join(binDirectory, cliBridgeCleanupRuntime));

function readBridgeManifest(path) {
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.size > maximumBridgeFileBytes) {
    fail("rootlight: deferred CLI cleanup manifest is invalid");
  }
  let value;
  try {
    value = JSON.parse(readFileSync(path, "utf8"));
  } catch {
    fail("rootlight: deferred CLI cleanup manifest is invalid");
  }
  if (
    value === null ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    value.schema !== cliBridgeSchema ||
    typeof value.packageRoot !== "string" ||
    !isAbsolute(value.packageRoot) ||
    typeof value.installPrefix !== "string" ||
    !isAbsolute(value.installPrefix) ||
    Object.keys(value).length !== 3
  ) {
    fail("rootlight: deferred CLI cleanup manifest is invalid");
  }
  return value;
}

function validateOwnedBridgeFile(path) {
  if (!existsSync(path)) {
    fail("rootlight: deferred CLI cleanup file is missing");
  }
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.size > maximumBridgeFileBytes) {
    fail("rootlight: deferred CLI cleanup ownership is invalid");
  }
  if (!readFileSync(path, "utf8").includes(cliBridgeMarker)) {
    fail("rootlight: deferred CLI cleanup file was modified");
  }
}

async function removeOwnedPath(path) {
  for (let attempt = 1; attempt <= maximumRemovalAttempts; attempt += 1) {
    try {
      rmSync(path, { force: true });
      if (!existsSync(path)) {
        return;
      }
    } catch (error) {
      if (attempt === maximumRemovalAttempts) {
        throw error;
      }
    }
    await delay(removalRetryDelayMilliseconds);
  }
  fail("rootlight: deferred CLI cleanup could not remove an owned file");
}

function bridgeFileNames() {
  const names = [".rootlight-cli-bridge.mjs", cliBridgeCleanupRuntime];
  for (const executable of publicExecutables) {
    names.push(executable, `${executable}.cmd`, `${executable}.ps1`);
  }
  return names;
}

function samePath(left, right) {
  const normalizeForPlatform = (value) => {
    let normalized;
    try {
      normalized = realpathSync.native(value);
    } catch {
      normalized = resolve(value);
    }
    return normalize(normalized).toLowerCase();
  };
  return normalizeForPlatform(left) === normalizeForPlatform(right);
}

function delay(milliseconds) {
  return new Promise((resolveDelay) => {
    setTimeout(resolveDelay, milliseconds);
  });
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

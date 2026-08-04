import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { dirname, isAbsolute, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const rootPackageName = "@tomasmarekk/rootlight";
const cliBridgeSchema = "rootlight.npm-cli-bridge/1";
const cliBridgeMarker = "rootlight npm cli bridge";
const cliBridgeManifest = ".rootlight-cli-bridge.json";
const cliBridgeRuntime = ".rootlight-cli-bridge.mjs";
const maximumBridgeFileBytes = 64 * 1024;
const nativePackages = new Map([
  ["darwin-arm64", "@tomasmarekk/rootlight-darwin-arm64"],
  ["darwin-x64", "@tomasmarekk/rootlight-darwin-x64"],
  ["linux-arm64", "@tomasmarekk/rootlight-linux-arm64-gnu"],
  ["linux-x64", "@tomasmarekk/rootlight-linux-x64-gnu"],
  ["win32-x64", "@tomasmarekk/rootlight-win32-x64-msvc"],
]);

const publicExecutables = new Set(["rootlight", "rootlight-mcp"]);

export function runRootlight() {
  if (process.argv.length === 3 && process.argv[2] === "uninstall") {
    uninstallRootlight();
  }
  runNative("rootlight");
}

export function runNative(executable) {
  if (!publicExecutables.has(executable)) {
    fail("rootlight: unsupported npm entry point");
  }
  exitFor(spawnNative(executable, process.argv.slice(2)));
}

export function runLifecycle(action) {
  if (!new Set(["install", "uninstall"]).has(action)) {
    fail("rootlight: unsupported npm lifecycle action");
  }
  if (action === "install") {
    try {
      installCliBridge();
    } catch (error) {
      fail(`rootlight: failed to install CLI access: ${errorMessage(error)}`);
    }
  }
  const result = spawnNative("rootlight", ["service", action], "ignore");
  if (result.error !== undefined) {
    if (action === "install") {
      removeCliBridgeAfterFailedInstall();
    }
    fail(`rootlight: failed to start the native executable: ${result.error.message}`);
  }
  if (result.status !== 0) {
    if (action === "install") {
      removeCliBridgeAfterFailedInstall();
    }
    fail(`rootlight: failed to ${action} the local service`);
  }
  if (action === "uninstall") {
    try {
      removeCliBridge();
    } catch (error) {
      fail(`rootlight: failed to remove CLI access: ${errorMessage(error)}`);
    }
  }
  process.exit(0);
}

function uninstallRootlight() {
  const cleanup = spawnNative("rootlight", ["service", "uninstall"]);
  if (cleanup.error !== undefined || cleanup.status !== 0) {
    exitFor(cleanup);
  }
  const context = packageInstallContext();
  const arguments_ =
    context.mode === "global"
      ? ["uninstall", "--global", rootPackageName, nativePackageForCurrentPlatform()]
      : [
          "uninstall",
          "--prefix",
          context.prefix,
          rootPackageName,
          nativePackageForCurrentPlatform(),
        ];
  const result = spawnNpm(arguments_, "inherit");
  if (result.error === undefined && result.status === 0) {
    try {
      removeCliBridge();
    } catch (error) {
      fail(`rootlight: failed to remove CLI access: ${errorMessage(error)}`);
    }
  }
  exitFor(result);
}

function spawnNative(executable, arguments_, stdio = "inherit") {
  const path = resolveNativeExecutable(executable);
  return spawnSync(path, arguments_, { stdio });
}

function resolveNativeExecutable(executable) {
  const nativePackage = nativePackageForCurrentPlatform();
  const require = createRequire(import.meta.url);
  let packageJsonPath;
  try {
    packageJsonPath = require.resolve(`${nativePackage}/package.json`);
  } catch {
    fail(
      `rootlight: native package ${nativePackage} is unavailable; reinstall on a supported glibc, macOS, or Windows host`,
    );
  }
  const rootPackage = readPackageJson(
    fileURLToPath(new URL("../package.json", import.meta.url)),
  );
  const nativePackageJson = readPackageJson(packageJsonPath);
  if (
    rootPackage.name !== rootPackageName ||
    nativePackageJson.name !== nativePackage ||
    nativePackageJson.version !== rootPackage.version
  ) {
    fail("rootlight: npm package versions do not match; reinstall Rootlight");
  }

  const suffix = process.platform === "win32" ? ".exe" : "";
  return join(dirname(packageJsonPath), "bin", `${executable}${suffix}`);
}

function installCliBridge() {
  const context = packageInstallContext();
  if (context.mode === "global") {
    removeSupersededLocalBridge(context.binDirectory);
    return;
  }
  ensureBinDirectoryOnPath(context.binDirectory);
  mkdirSync(context.binDirectory, { recursive: true });
  const manifestPath = join(context.binDirectory, cliBridgeManifest);
  const existingManifest = readBridgeManifest(manifestPath);
  const ownedFiles = bridgeFileNames();
  for (const name of ownedFiles) {
    const path = join(context.binDirectory, name);
    if (!existsSync(path)) {
      continue;
    }
    if (existingManifest === undefined) {
      throw new Error(`command collision at ${path}`);
    }
    validateOwnedBridgeFile(path);
  }

  const targets = Object.fromEntries(
    [...publicExecutables].map((executable) => [
      executable,
      fileURLToPath(new URL(`${executable}.mjs`, import.meta.url)),
    ]),
  );
  writeBridgeFile(
    join(context.binDirectory, cliBridgeRuntime),
    [
      "#!/usr/bin/env node",
      `// ${cliBridgeMarker}`,
      'import { pathToFileURL } from "node:url";',
      `const targets = ${JSON.stringify(targets)};`,
      "const [, , executable, ...arguments_] = process.argv;",
      "const target = targets[executable];",
      'if (target === undefined) { console.error("rootlight: unsupported CLI bridge target"); process.exit(1); }',
      "process.argv = [process.argv[0], target, ...arguments_];",
      "await import(pathToFileURL(target));",
      "",
    ].join("\n"),
  );
  for (const executable of publicExecutables) {
    writeExecutableShims(context.binDirectory, executable);
  }
  writeBridgeFile(
    manifestPath,
    `${JSON.stringify(
      {
        schema: cliBridgeSchema,
        packageRoot: packageRoot(),
        installPrefix: context.prefix,
      },
      null,
      2,
    )}\n`,
    false,
  );
}

function removeCliBridgeAfterFailedInstall() {
  try {
    removeCliBridge();
  } catch {
    // The primary lifecycle failure remains the actionable npm diagnostic.
  }
}

function removeCliBridge() {
  const context = packageInstallContext();
  const manifestPath = join(context.binDirectory, cliBridgeManifest);
  const manifest = readBridgeManifest(manifestPath);
  if (manifest === undefined || !samePath(manifest.packageRoot, packageRoot())) {
    return;
  }
  for (const name of bridgeFileNames()) {
    const path = join(context.binDirectory, name);
    if (!existsSync(path)) {
      continue;
    }
    validateOwnedBridgeFile(path);
  }
  for (const name of bridgeFileNames()) {
    rmSync(join(context.binDirectory, name), { force: true });
  }
  rmSync(manifestPath, { force: true });
}

function removeSupersededLocalBridge(binDirectory) {
  const manifestPath = join(binDirectory, cliBridgeManifest);
  const manifest = readBridgeManifest(manifestPath);
  if (manifest === undefined) {
    return;
  }
  const runtimePath = join(binDirectory, cliBridgeRuntime);
  if (existsSync(runtimePath)) {
    validateOwnedBridgeFile(runtimePath);
    rmSync(runtimePath);
  }
  rmSync(manifestPath);
}

function writeExecutableShims(binDirectory, executable) {
  const runtimeName = cliBridgeRuntime;
  const shell = [
    "#!/bin/sh",
    `# ${cliBridgeMarker}`,
    `exec node "$(dirname "$0")/${runtimeName}" ${executable} "$@"`,
    "",
  ].join("\n");
  writeBridgeFile(join(binDirectory, executable), shell);
  if (process.platform !== "win32") {
    return;
  }
  writeBridgeFile(
    join(binDirectory, `${executable}.cmd`),
    [
      "@ECHO off",
      `REM ${cliBridgeMarker}`,
      `node "%~dp0\\${runtimeName}" ${executable} %*`,
      "",
    ].join("\r\n"),
  );
  writeBridgeFile(
    join(binDirectory, `${executable}.ps1`),
    [
      `# ${cliBridgeMarker}`,
      `& node "$PSScriptRoot\\${runtimeName}" ${executable} @args`,
      "exit $LASTEXITCODE",
      "",
    ].join("\r\n"),
  );
}

function writeBridgeFile(path, content, executable = true) {
  const mode = executable ? 0o755 : 0o600;
  writeFileSync(path, content, { encoding: "utf8", mode });
  if (process.platform !== "win32") {
    chmodSync(path, mode);
  }
}

function validateOwnedBridgeFile(path) {
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.size > maximumBridgeFileBytes) {
    throw new Error(`CLI bridge ownership is invalid at ${path}`);
  }
  if (!readFileSync(path, "utf8").includes(cliBridgeMarker)) {
    throw new Error(`CLI bridge was modified at ${path}`);
  }
}

function readBridgeManifest(path) {
  if (!existsSync(path)) {
    return undefined;
  }
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.size > maximumBridgeFileBytes) {
    throw new Error(`CLI bridge manifest is invalid at ${path}`);
  }
  let value;
  try {
    value = JSON.parse(readFileSync(path, "utf8"));
  } catch {
    throw new Error(`CLI bridge manifest is invalid at ${path}`);
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
    throw new Error(`CLI bridge manifest is invalid at ${path}`);
  }
  return value;
}

function bridgeFileNames() {
  const names = [cliBridgeRuntime];
  for (const executable of publicExecutables) {
    names.push(executable);
    if (process.platform === "win32") {
      names.push(`${executable}.cmd`, `${executable}.ps1`);
    }
  }
  return names;
}

function packageInstallContext() {
  const prefix = npmGlobalPrefix();
  const binDirectory =
    process.platform === "win32" ? prefix : join(prefix, "bin");
  const globalPackageRoot =
    process.platform === "win32"
      ? join(prefix, "node_modules", "@tomasmarekk", "rootlight")
      : join(prefix, "lib", "node_modules", "@tomasmarekk", "rootlight");
  if (samePath(packageRoot(), globalPackageRoot)) {
    return { mode: "global", prefix, binDirectory };
  }
  const nodeModules = dirname(dirname(packageRoot()));
  if (
    !samePath(packageRoot(), join(nodeModules, "@tomasmarekk", "rootlight")) ||
    normalize(nodeModules).split(/[\\/]/u).at(-1) !== "node_modules"
  ) {
    throw new Error("npm installation layout is unsupported");
  }
  return {
    mode: "local",
    prefix: dirname(nodeModules),
    binDirectory,
  };
}

function packageRoot() {
  return dirname(fileURLToPath(new URL("../package.json", import.meta.url)));
}

function npmGlobalPrefix() {
  const configured =
    process.env.npm_config_prefix ?? process.env.NPM_CONFIG_PREFIX;
  if (configured !== undefined && configured.length > 0 && isAbsolute(configured)) {
    return resolve(configured);
  }
  const result = spawnNpm(["prefix", "--global"], "pipe");
  if (result.error !== undefined) {
    throw result.error;
  }
  if (result.status !== 0 || typeof result.stdout !== "string") {
    throw new Error("npm global prefix is unavailable");
  }
  const prefix = result.stdout.trim();
  if (!isAbsolute(prefix)) {
    throw new Error("npm global prefix is invalid");
  }
  return resolve(prefix);
}

function spawnNpm(arguments_, stdio) {
  return spawnSync("npm", arguments_, {
    encoding: stdio === "pipe" ? "utf8" : undefined,
    stdio,
  });
}

function ensureBinDirectoryOnPath(binDirectory) {
  const pathEntries = (process.env.PATH ?? "")
    .split(process.platform === "win32" ? ";" : ":")
    .filter((entry) => entry.length > 0);
  if (!pathEntries.some((entry) => samePath(entry, binDirectory))) {
    throw new Error(
      `npm global command directory ${binDirectory} is not on PATH; configure npm's global prefix before installing Rootlight`,
    );
  }
}

function samePath(left, right) {
  const normalizeForPlatform = (value) => {
    let normalized;
    try {
      normalized = realpathSync.native(value);
    } catch {
      normalized = resolve(value);
    }
    normalized = normalize(normalized);
    return process.platform === "win32" ? normalized.toLowerCase() : normalized;
  };
  return normalizeForPlatform(left) === normalizeForPlatform(right);
}

function nativePackageForCurrentPlatform() {
  const platformKey = `${process.platform}-${process.arch}`;
  const nativePackage = nativePackages.get(platformKey);
  if (nativePackage === undefined) {
    fail(`rootlight: unsupported npm platform ${process.platform}/${process.arch}`);
  }
  return nativePackage;
}

function readPackageJson(path) {
  try {
    const value = JSON.parse(readFileSync(path, "utf8"));
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      fail("rootlight: npm package metadata is invalid");
    }
    return value;
  } catch {
    fail("rootlight: npm package metadata is unavailable");
  }
}

function exitFor(result) {
  if (result.error !== undefined) {
    fail(`rootlight: failed to start the native executable: ${result.error.message}`);
  }
  process.exit(result.status ?? 1);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

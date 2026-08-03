import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const rootPackageName = "@tomasmarekk/rootlight";
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
  exitFor(spawnNative("rootlight", ["service", action]));
}

function uninstallRootlight() {
  const cleanup = spawnNative("rootlight", ["service", "uninstall"]);
  if (cleanup.error !== undefined || cleanup.status !== 0) {
    exitFor(cleanup);
  }
  const result =
    process.platform === "win32"
      ? spawnSync(
          process.env.ComSpec ?? "cmd.exe",
          ["/d", "/s", "/c", `npm.cmd uninstall --global ${rootPackageName}`],
          { stdio: "inherit" },
        )
      : spawnSync("npm", ["uninstall", "--global", rootPackageName], {
          stdio: "inherit",
        });
  exitFor(result);
}

function spawnNative(executable, arguments_) {
  const path = resolveNativeExecutable(executable);
  return spawnSync(path, arguments_, { stdio: "inherit" });
}

function resolveNativeExecutable(executable) {
  const platformKey = `${process.platform}-${process.arch}`;
  const nativePackage = nativePackages.get(platformKey);
  if (nativePackage === undefined) {
    fail(`rootlight: unsupported npm platform ${process.platform}/${process.arch}`);
  }

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

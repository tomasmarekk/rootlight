import assert from "node:assert/strict";
import {
  copyFileSync,
  existsSync,
  linkSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";

const nativePackages = new Map([
  ["darwin-arm64", "@tomasmarekk/rootlight-darwin-arm64"],
  ["darwin-x64", "@tomasmarekk/rootlight-darwin-x64"],
  ["linux-arm64", "@tomasmarekk/rootlight-linux-arm64-gnu"],
  ["linux-x64", "@tomasmarekk/rootlight-linux-x64-gnu"],
  ["win32-x64", "@tomasmarekk/rootlight-win32-x64-msvc"],
]);

const nativePackage = nativePackages.get(`${process.platform}-${process.arch}`);
assert.notEqual(nativePackage, undefined, "test host must be supported");

const sourceRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const temporary = mkdtempSync(join(tmpdir(), "rootlight-npm-wrappers-"));
try {
  const globalPrefix = join(temporary, "global");
  const globalBin =
    process.platform === "win32" ? globalPrefix : join(globalPrefix, "bin");
  const scope = join(temporary, "node_modules", "@tomasmarekk");
  const rootBin = join(scope, "rootlight", "bin");
  const nativeRoot = join(scope, nativePackage.split("/")[1]);
  const nativeBin = join(nativeRoot, "bin");
  mkdirSync(rootBin, { recursive: true });
  mkdirSync(nativeBin, { recursive: true });
  writeFileSync(
    join(scope, "rootlight", "package.json"),
    `${JSON.stringify({ name: "@tomasmarekk/rootlight", version: "0.0.0" })}\n`,
  );

  for (const file of [
    "rootlight.mjs",
    "rootlight-mcp.mjs",
    "run-native.mjs",
    "remove-cli-bridge.mjs",
    "postinstall.mjs",
    "preuninstall.mjs",
  ]) {
    copyFileSync(
      join(sourceRoot, "packaging", "npm", file),
      join(rootBin, file),
    );
  }
  writeFileSync(
    join(nativeRoot, "package.json"),
    `${JSON.stringify({ name: nativePackage, version: "0.0.0" })}\n`,
  );
  writeFileSync(
    join(temporary, "service"),
    [
      'const { spawn } = require("node:child_process");',
      'if (process.env.ROOTLIGHT_TEST_FORBID_SERVICE === "1") process.exit(97);',
      'if (!new Set(["install", "uninstall"]).has(process.argv[2])) process.exit(1);',
      'const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 3000)"], { stdio: "inherit" });',
      "child.unref();",
      "",
    ].join("\n"),
  );

  const suffix = process.platform === "win32" ? ".exe" : "";
  for (const executable of ["rootlight", "rootlight-mcp"]) {
    const nativeExecutable = join(nativeBin, `${executable}${suffix}`);
    linkSync(process.execPath, nativeExecutable);

    const result = spawnSync(
      process.execPath,
      [join(rootBin, `${executable}.mjs`), "--version"],
      { encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), process.version);
    assert.equal(result.stderr, "");
  }

  const lifecycleEnvironment = {
    ...process.env,
    npm_config_prefix: globalPrefix,
    PATH: [globalBin, process.env.PATH ?? ""].join(
      process.platform === "win32" ? ";" : ":",
    ),
  };
  const elevatedLifecycle = spawnSync(
    process.execPath,
    [
      "--input-type=module",
      "--eval",
      `process.getuid = () => 0; await import(${JSON.stringify(
        pathToFileURL(join(rootBin, "postinstall.mjs")).href,
      )});`,
    ],
    {
      cwd: temporary,
      encoding: "utf8",
      env: {
        ...lifecycleEnvironment,
        ROOTLIGHT_TEST_FORBID_SERVICE: "1",
      },
    },
  );
  assert.equal(elevatedLifecycle.status, 0, elevatedLifecycle.stderr);
  assert.match(elevatedLifecycle.stderr, /refusing to install.* as root/u);

  for (const lifecycle of ["postinstall", "preuninstall"]) {
    const started = Date.now();
    const result = spawnSync(
      process.execPath,
      [join(rootBin, `${lifecycle}.mjs`)],
      {
        cwd: temporary,
        encoding: "utf8",
        env: lifecycleEnvironment,
      },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.ok(
      Date.now() - started < 2000,
      `${lifecycle} waited for a persistent descendant`,
    );
    assert.equal(result.stdout, "");
    assert.equal(result.stderr, "");
    if (lifecycle === "postinstall") {
      const manifest = JSON.parse(
        readFileSync(join(globalBin, ".rootlight-cli-bridge.json"), "utf8"),
      );
      assert.equal(manifest.schema, "rootlight.npm-cli-bridge/1");
      assert.equal(manifest.installPrefix, temporary);
      const bridgeResult = spawnSync(
        process.execPath,
        [
          join(globalBin, ".rootlight-cli-bridge.mjs"),
          "rootlight",
          "--version",
        ],
        { encoding: "utf8", env: lifecycleEnvironment },
      );
      assert.equal(bridgeResult.status, 0, bridgeResult.stderr);
      assert.equal(bridgeResult.stdout.trim(), process.version);
    }
  }

  for (const name of [
    ".rootlight-cli-bridge.json",
    ".rootlight-cli-bridge.mjs",
    ".rootlight-cli-cleanup.mjs",
    "rootlight",
    "rootlight-mcp",
    ...(process.platform === "win32"
      ? [
          "rootlight.cmd",
          "rootlight.ps1",
          "rootlight-mcp.cmd",
          "rootlight-mcp.ps1",
        ]
      : []),
  ]) {
    assert.equal(existsSync(join(globalBin, name)), false, `${name} remained`);
  }

  if (process.platform === "win32") {
    const reinstall = spawnSync(
      process.execPath,
      [join(rootBin, "postinstall.mjs")],
      {
        cwd: temporary,
        encoding: "utf8",
        env: lifecycleEnvironment,
      },
    );
    assert.equal(reinstall.status, 0, reinstall.stderr);
    const managedPreuninstall = spawnSync(
      process.execPath,
      [join(rootBin, "preuninstall.mjs")],
      {
        cwd: temporary,
        encoding: "utf8",
        env: {
          ...lifecycleEnvironment,
          ROOTLIGHT_NPM_MANAGED_UNINSTALL: "1",
        },
      },
    );
    assert.equal(managedPreuninstall.status, 0, managedPreuninstall.stderr);
    const cleanupRuntime = join(globalBin, ".rootlight-cli-cleanup.mjs");
    assert.equal(existsSync(cleanupRuntime), true);
    const deferredCleanup = spawnSync(
      process.execPath,
      [cleanupRuntime, join(scope, "rootlight"), temporary, globalBin],
      { encoding: "utf8", env: lifecycleEnvironment },
    );
    assert.equal(deferredCleanup.status, 0, deferredCleanup.stderr);
    assert.equal(existsSync(cleanupRuntime), false);
    assert.equal(existsSync(join(globalBin, "rootlight.cmd")), false);
  }

  mkdirSync(globalBin, { recursive: true });
  const collision = join(globalBin, "rootlight");
  writeFileSync(collision, "foreign command\n");
  const collisionResult = spawnSync(
    process.execPath,
    [join(rootBin, "postinstall.mjs")],
    {
      cwd: temporary,
      encoding: "utf8",
      env: lifecycleEnvironment,
    },
  );
  assert.notEqual(collisionResult.status, 0);
  assert.match(collisionResult.stderr, /command collision/u);
  assert.equal(readFileSync(collision, "utf8"), "foreign command\n");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}

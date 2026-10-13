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
import * as fs from "node:fs";
import { runInNewContext } from "node:vm";

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

function createNodeRuntime(root, npmCliSource) {
  const nodeExecutable =
    process.platform === "win32"
      ? join(root, "node.exe")
      : join(root, "bin", "node");
  mkdirSync(dirname(nodeExecutable), { recursive: true });
  linkSync(process.execPath, nodeExecutable);
  if (npmCliSource !== undefined) {
    const npmCli =
      process.platform === "win32"
        ? join(root, "node_modules", "npm", "bin", "npm-cli.js")
        : join(root, "lib", "node_modules", "npm", "bin", "npm-cli.js");
    mkdirSync(dirname(npmCli), { recursive: true });
    writeFileSync(npmCli, npmCliSource);
  }
  return nodeExecutable;
}

function npmResolutionEnvironment(overrides = {}) {
  const environment = { ...process.env };
  delete environment.npm_config_prefix;
  delete environment.NPM_CONFIG_PREFIX;
  delete environment.npm_execpath;
  delete environment.NPM_EXECPATH;
  return { ...environment, ...overrides };
}

function markerNpmCli(marker, prefix) {
  return [
    'const { writeFileSync } = require("node:fs");',
    `writeFileSync(${JSON.stringify(marker)}, "executed\\n");`,
    `console.log(${JSON.stringify(prefix)});`,
    "",
  ].join("\n");
}

function bridgeReaderSource(file) {
  const source = readFileSync(join(sourceRoot, "packaging", "npm", file), "utf8");
  const matches = [...source.matchAll(/^function readBridgeFile\(path\) \{[\s\S]*?^\}/gmu)];
  assert.equal(matches.length, 1, `${file} must contain one standalone bridge reader`);
  return matches[0][0];
}

function instrumentBridgeReader(source, hooks = {}) {
  const calls = { opened: [], closed: [], allocations: 0, reads: 0 };
  const reader = runInNewContext(`(${source})`, {
    ...fs,
    ...hooks,
    maximumBridgeFileBytes: 64 * 1024,
    Buffer: {
      alloc(size) {
        calls.allocations += 1;
        assert.ok(size <= 64 * 1024 + 1, "bridge allocation must remain bounded");
        return Buffer.alloc(size);
      },
    },
    openSync(...args) {
      const descriptor = (hooks.openSync ?? fs.openSync)(...args);
      calls.opened.push(descriptor);
      return descriptor;
    },
    closeSync(descriptor) {
      calls.closed.push(descriptor);
      fs.closeSync(descriptor);
    },
    readSync(...args) {
      calls.reads += 1;
      return (hooks.readSync ?? fs.readSync)(...args);
    },
  });
  return { reader, calls };
}

function assertBridgeReaders(temporary) {
  const sources = ["run-native.mjs", "remove-cli-bridge.mjs"].map(bridgeReaderSource);
  assert.equal(sources[0], sources[1], "standalone cleanup must retain the same guard");
  for (const [index, source] of sources.entries()) {
    const directory = join(temporary, `reader-${index}`);
    mkdirSync(directory);
    const path = join(directory, "bridge");
    const assertRejectedBeforeRead = (hooks = {}, expected) => {
      const { reader, calls } = instrumentBridgeReader(source, hooks);
      assert.throws(() => reader(path), expected);
      assert.equal(calls.allocations, 0);
      assert.equal(calls.reads, 0);
      assert.deepEqual(calls.closed, calls.opened);
    };
    for (const size of [0, 64 * 1024]) {
      writeFileSync(path, "x".repeat(size));
      const { reader, calls } = instrumentBridgeReader(source);
      assert.equal(reader(path), "x".repeat(size));
      assert.deepEqual(calls.closed, calls.opened);
      assert.equal(calls.opened.length, 1);
    }
    writeFileSync(path, "x".repeat(64 * 1024 + 1));
    assertRejectedBeforeRead();
    rmSync(path);
    mkdirSync(path);
    assertRejectedBeforeRead();
    rmSync(path, { recursive: true });

    for (const replacement of ["other bridge", "x".repeat(64 * 1024 + 1)]) {
      writeFileSync(path, "owned bridge");
      assertRejectedBeforeRead(
        {
          openSync(file, flags) {
            const descriptor = fs.openSync(file, flags);
            fs.renameSync(file, join(directory, `opened-${replacement.length}`));
            writeFileSync(file, replacement);
            return descriptor;
          },
        },
        replacement.length > 64 * 1024 ? /bounded regular file/u : /changed while it was opened/u,
      );
    }
    writeFileSync(path, "owned bridge");
    assertRejectedBeforeRead(
      { lstatSync() { throw new Error("metadata unavailable"); } },
      /metadata unavailable/u,
    );
    for (const mutate of [() => fs.appendFileSync(path, "x"), () => fs.truncateSync(path, 0)]) {
      writeFileSync(path, "owned bridge");
      let changed = false;
      const { reader, calls } = instrumentBridgeReader(source, {
        readSync(...args) {
          if (!changed) {
            changed = true;
            mutate();
          }
          return fs.readSync(...args);
        },
      });
      assert.throws(() => reader(path), /changed while it was read/u);
      assert.deepEqual(calls.closed, calls.opened);
      assert.equal(calls.opened.length, 1);
    }

    const destination = join(directory, "destination");
    writeFileSync(destination, "foreign bridge");
    rmSync(path);
    try {
      fs.symlinkSync(destination, path, "file");
    } catch (error) {
      if (process.platform !== "win32" || error.code !== "EPERM") throw error;
      console.log("bridge symlink test requires Windows symlink privilege");
    }
    if (fs.existsSync(path)) {
      assertRejectedBeforeRead();
      assertRejectedBeforeRead({ constants: { ...fs.constants, O_NOFOLLOW: 0 } });
      assert.equal(readFileSync(destination, "utf8"), "foreign bridge");
      rmSync(path);
    }
    if (process.platform !== "win32") {
      writeFileSync(path, "owned bridge");
      const fifo = join(directory, "fifo");
      const created = spawnSync("mkfifo", [fifo], { encoding: "utf8" });
      assert.equal(created.status, 0, created.stderr);
      // A separate process bounds a regression that blocks opening the substituted FIFO.
      const result = spawnSync(process.execPath, ["--input-type=module", "--eval", `
        import assert from "node:assert/strict";
        import * as fs from "node:fs";
        import { runInNewContext } from "node:vm";
        const source = ${JSON.stringify(source)};
        const path = ${JSON.stringify(path)};
        const fifo = ${JSON.stringify(fifo)};
        let opened = 0;
        let closed = 0;
        const reader = runInNewContext("(" + source + ")", {
          ...fs, maximumBridgeFileBytes: 64 * 1024,
          Buffer: { alloc() { assert.fail("FIFO must not allocate"); } },
          openSync(file, flags) {
            fs.renameSync(fifo, file);
            const descriptor = fs.openSync(file, flags);
            opened += 1;
            return descriptor;
          },
          readSync() { assert.fail("FIFO must not be read"); },
          closeSync(descriptor) { closed += 1; fs.closeSync(descriptor); },
        });
        assert.throws(() => reader(path), /bounded regular file/u);
        assert.equal(opened, 1);
        assert.equal(closed, 1);
      `], { encoding: "utf8", timeout: 5000 });
      assert.equal(result.error, undefined, result.error?.message);
      assert.equal(result.status, 0, result.stderr);
    }
  }
}

const temporary = mkdtempSync(join(tmpdir(), "rootlight-npm-wrappers-"));
try {
  assertBridgeReaders(temporary);
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
      'if (process.argv[2] === "status") {',
      '  console.log(JSON.stringify({ contract_version: "1.0", ok: true, result: { type: "web_service", data: { registered: false, running: false, pid: null } } }));',
      "  process.exit(0);",
      "}",
      'if (!new Set(["install", "uninstall"]).has(process.argv[2])) process.exit(1);',
      'if (process.env.ROOTLIGHT_TEST_ELEVATED_SERVICE === "1") {',
      '  console.error(JSON.stringify({ contract_version: "1.0", ok: false, exit_family: "security_policy", error: { code: "PERMISSION_DENIED", message: "local web service requires a non-elevated process", retryable: false } }));',
      "  process.exit(6);",
      "}",
      "if (process.env.ROOTLIGHT_TEST_SERVICE_FAILURE) {",
      "  const reason = process.env.ROOTLIGHT_TEST_SERVICE_FAILURE;",
      '  const privateValue = "private-source-and-bootstrap-capability";',
      '  if (reason === "descendant") { const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 3000)"], { stdio: "inherit" }); child.unref(); }',
      '  const label = new Set(["large", "descendant"]).has(reason) ? "early_exit" : reason;',
      '  const output = reason === "raw" ? privateValue : reason === "oversized" ? privateValue.repeat(4000) : JSON.stringify({ contract_version: "1.0", ok: false, error: { code: "BUSY", details: { service_reason: { type: "label", value: label } }, unexpected: reason === "large" ? privateValue.repeat(1600) : privateValue } });',
      '  require("node:fs").writeSync(2, output);',
      "  process.exit(3);",
      "}",
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

  if (process.platform === "win32") {
    const elevatedWindowsLifecycle = spawnSync(
      process.execPath,
      [join(rootBin, "postinstall.mjs")],
      {
        cwd: temporary,
        encoding: "utf8",
        env: {
          ...lifecycleEnvironment,
          ROOTLIGHT_TEST_ELEVATED_SERVICE: "1",
        },
      },
    );
    assert.equal(
      elevatedWindowsLifecycle.status,
      0,
      elevatedWindowsLifecycle.stderr,
    );
    assert.match(
      elevatedWindowsLifecycle.stderr,
      /refusing to install.*elevated Windows process/u,
    );
  }

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

  for (const [nativeReason, expectedReason] of [
    ["early_exit", "early_exit"],
    ["daemon_start_timeout", "daemon_start_timeout"],
    ["daemon_unavailable", "daemon_unavailable"],
    ["registration_failed", "registration_failed"],
    ["large", "early_exit"],
    ["descendant", "early_exit"],
    ["private-source-and-bootstrap-capability", "unclassified"],
    ["raw", "unclassified"],
    ["oversized", "output_limit"],
  ]) {
    const started = Date.now();
    const result = spawnSync(
      process.execPath,
      [join(rootBin, "postinstall.mjs")],
      {
        cwd: temporary,
        encoding: "utf8",
        env: {
          ...lifecycleEnvironment,
          ROOTLIGHT_TEST_SERVICE_FAILURE: nativeReason,
        },
      },
    );
    assert.equal(result.status, 1, result.stderr);
    assert.ok(Date.now() - started < 2000, "failed lifecycle waited for a descendant");
    assert.equal(result.stdout, "");
    assert.ok(
      !result.stderr.includes("private-source-and-bootstrap-capability"),
    );
    const diagnosticLine = result.stderr
      .split("\n")
      .find((line) => line.startsWith("rootlight: service failure "));
    assert.notEqual(diagnosticLine, undefined, result.stderr);
    assert.deepEqual(
      JSON.parse(diagnosticLine.slice("rootlight: service failure ".length)),
      {
        schema: "rootlight.npm-service-failure/1",
        action: "install",
        reason: expectedReason,
        exit_status: 3,
        signal: null,
      },
    );
    assert.equal(
      existsSync(join(globalBin, ".rootlight-cli-bridge.json")),
      false,
    );
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

  const untrustedRuntime = createNodeRuntime(
    join(temporary, "untrusted-node"),
  );
  const npmExecpathMarker = join(temporary, "npm-execpath-executed");
  const maliciousNpmExecpath = join(temporary, "malicious-npm-cli.js");
  writeFileSync(
    maliciousNpmExecpath,
    markerNpmCli(npmExecpathMarker, globalPrefix),
  );
  const npmExecpathResult = spawnSync(
    untrustedRuntime,
    [join(rootBin, "postinstall.mjs")],
    {
      cwd: temporary,
      encoding: "utf8",
      env: npmResolutionEnvironment({
        npm_execpath: maliciousNpmExecpath,
        PATH: process.env.PATH ?? "",
      }),
    },
  );
  assert.notEqual(npmExecpathResult.status, 0);
  assert.match(npmExecpathResult.stderr, /npm CLI is unavailable/u);
  assert.equal(existsSync(npmExecpathMarker), false);

  const pathMarker = join(temporary, "path-npm-cli-executed");
  const maliciousPathEntry = join(temporary, "malicious-path");
  const maliciousPathNpmCli = join(
    maliciousPathEntry,
    "node_modules",
    "npm",
    "bin",
    "npm-cli.js",
  );
  mkdirSync(dirname(maliciousPathNpmCli), { recursive: true });
  writeFileSync(maliciousPathNpmCli, markerNpmCli(pathMarker, globalPrefix));
  const pathResult = spawnSync(
    untrustedRuntime,
    [join(rootBin, "postinstall.mjs")],
    {
      cwd: temporary,
      encoding: "utf8",
      env: npmResolutionEnvironment({
        PATH: [maliciousPathEntry, process.env.PATH ?? ""].join(
          process.platform === "win32" ? ";" : ":",
        ),
      }),
    },
  );
  assert.notEqual(pathResult.status, 0);
  assert.match(pathResult.stderr, /npm CLI is unavailable/u);
  assert.equal(existsSync(pathMarker), false);

  const trustedMarker = join(temporary, "trusted-npm-cli-executed");
  const trustedRuntime = createNodeRuntime(
    join(temporary, "trusted-node"),
    markerNpmCli(trustedMarker, globalPrefix),
  );
  const trustedEnvironment = npmResolutionEnvironment({
    npm_execpath: maliciousNpmExecpath,
    PATH: [globalBin, maliciousPathEntry, process.env.PATH ?? ""].join(
      process.platform === "win32" ? ";" : ":",
    ),
  });
  const trustedResult = spawnSync(
    trustedRuntime,
    [join(rootBin, "postinstall.mjs")],
    {
      cwd: temporary,
      encoding: "utf8",
      env: trustedEnvironment,
    },
  );
  assert.equal(trustedResult.status, 0, trustedResult.stderr);
  assert.equal(existsSync(trustedMarker), true);
  assert.equal(existsSync(npmExecpathMarker), false);
  assert.equal(existsSync(pathMarker), false);
  assert.equal(
    existsSync(join(globalBin, ".rootlight-cli-bridge.json")),
    true,
  );
  const trustedCleanup = spawnSync(
    trustedRuntime,
    [join(rootBin, "preuninstall.mjs")],
    {
      cwd: temporary,
      encoding: "utf8",
      env: trustedEnvironment,
    },
  );
  assert.equal(trustedCleanup.status, 0, trustedCleanup.stderr);
  assert.equal(
    existsSync(join(globalBin, ".rootlight-cli-bridge.json")),
    false,
  );

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

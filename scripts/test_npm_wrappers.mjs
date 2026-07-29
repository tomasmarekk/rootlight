import assert from "node:assert/strict";
import {
  copyFileSync,
  linkSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const nativePackages = new Map([
  ["darwin-arm64", "@tomasmarekk/rootlight-darwin-arm64"],
  ["darwin-x64", "@tomasmarekk/rootlight-darwin-x64"],
  ["linux-arm64", "@tomasmarekk/rootlight-linux-arm64-gnu"],
  ["linux-x64", "@tomasmarekk/rootlight-linux-x64-gnu"],
  ["win32-x64", "@tomasmarekk/rootlight-win32-x64-msvc"],
]);

const nativePackage = nativePackages.get(
  `${process.platform}-${process.arch}`,
);
assert.notEqual(nativePackage, undefined, "test host must be supported");

const sourceRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const temporary = mkdtempSync(join(tmpdir(), "rootlight-npm-wrappers-"));
try {
  const scope = join(temporary, "node_modules", "@tomasmarekk");
  const rootBin = join(scope, "rootlight", "bin");
  const nativeRoot = join(scope, nativePackage.split("/")[1]);
  const nativeBin = join(nativeRoot, "bin");
  mkdirSync(rootBin, { recursive: true });
  mkdirSync(nativeBin, { recursive: true });

  for (const file of [
    "rootlight.mjs",
    "rootlight-mcp.mjs",
    "run-native.mjs",
  ]) {
    copyFileSync(join(sourceRoot, "packaging", "npm", file), join(rootBin, file));
  }
  writeFileSync(
    join(nativeRoot, "package.json"),
    `${JSON.stringify({ name: nativePackage, version: "0.0.0" })}\n`,
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
} finally {
  rmSync(temporary, { recursive: true, force: true });
}

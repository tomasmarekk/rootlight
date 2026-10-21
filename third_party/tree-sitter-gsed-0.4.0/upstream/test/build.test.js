import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import {
  copyFiles,
  createTreeSitter,
  packageName,
  root,
} from "../scripts/tree-sitter.js";

const commandTimeoutMs = 60_000;
const archiveTimeoutMs = 120_000;
const archiveOutputLimitBytes = 64 * 1024 * 1024;

const configuration = JSON.parse(
  readFileSync(join(root, "tree-sitter.json"), "utf8"),
);
const grammars = configuration.grammars.map((grammar) => ({
  ...grammar,
  externalFiles: [].concat(grammar["external-files"] ?? []),
}));

const language = packageName.slice("tree-sitter-".length).replaceAll("-", "_");

test(`${language}: generated checks reject stale and missing files without rewriting them`, () => {
  const cache = join(root, "node_modules", ".cache");
  mkdirSync(cache, { recursive: true });
  const directory = mkdtempSync(join(cache, `${packageName}-generated-check-`));
  try {
    copyFiles(
      [
        "package.json",
        "tree-sitter.json",
        "scripts",
        ...(existsSync(join(root, "common")) ? ["common"] : []),
        ...grammars.flatMap(({ path, externalFiles }) => [
          join(path, "grammar.js"),
          join(path, "src"),
          ...externalFiles,
        ]),
      ],
      directory,
    );

    function check() {
      const result = spawnSync(
        process.execPath,
        ["scripts/check-generated.js"],
        {
          cwd: directory,
          encoding: "utf8",
          timeout: commandTimeoutMs,
          killSignal: "SIGKILL",
        },
      );
      assert.ifError(result.error);
      return result;
    }

    const clean = check();
    assert.equal(clean.status, 0, clean.stdout + clean.stderr);
    const changedFiles = [];
    const missingFiles = [];
    for (const { path } of grammars) {
      const nodeTypes = join(path, "src", "node-types.json");
      const changed = `${readFileSync(join(directory, nodeTypes), "utf8")}\n`;
      writeFileSync(join(directory, nodeTypes), changed);
      changedFiles.push([nodeTypes, changed]);
      const parser = join(path, "src", "parser.c");
      rmSync(join(directory, parser));
      missingFiles.push(parser);
    }

    const stale = check();
    assert.equal(stale.status, 1, stale.stdout + stale.stderr);
    for (const [path, expected] of changedFiles) {
      assert.ok(stale.stderr.includes(path), stale.stderr);
      assert.equal(readFileSync(join(directory, path), "utf8"), expected);
    }
    for (const path of missingFiles) {
      assert.ok(stale.stderr.includes(path), stale.stderr);
      assert.equal(existsSync(join(directory, path)), false);
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test(`${language}: corpus fuzz propagates CLI failures even when its exit status is zero`, () => {
  const directory = mkdtempSync(join(tmpdir(), "tree-sitter-fuzz-exit-#-"));
  const preload = join(directory, "cli.mjs");
  const script = join(import.meta.dirname, "..", "scripts", "tree-sitter.js");
  const fixtures = [
    {
      name: "successful CLI output",
      status: 0,
      stdout: "0 test_language corpus tests failed fuzzing\n",
      stderr: "",
      expectedStatus: 0,
    },
    {
      name: "failed fuzz case with successful CLI exit status",
      status: 0,
      stdout: "1 test_language corpus tests failed fuzzing\n",
      stderr: "",
      expectedStatus: 1,
    },
    {
      name: "failed CLI exit status",
      status: 1,
      stdout: "",
      stderr: "fuzz command failed\n",
      expectedStatus: 1,
    },
    {
      name: "signal termination retains its cause",
      status: null,
      signal: "SIGTERM",
      stdout: "fuzz progress\n",
      stderr: "",
      expectedStatus: 1,
      expectedDiagnostic: "Tree-sitter CLI terminated by SIGTERM.\n",
    },
    {
      name: "timeout retains the captured output",
      status: null,
      signal: "SIGKILL",
      error: { code: "ETIMEDOUT", message: "spawnSync tree-sitter ETIMEDOUT" },
      stdout: "fuzz progress\n",
      stderr: "fuzz log\n",
      expectedStatus: 1,
      expectedDiagnostic: "spawnSync tree-sitter ETIMEDOUT\n",
    },
  ];
  try {
    for (const fixture of fixtures) {
      writeFileSync(
        preload,
        `
import childProcess from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
const fixture = ${JSON.stringify(fixture)};
if (fixture.error) {
  fixture.error = Object.assign(new Error(fixture.error.message), fixture.error);
}
childProcess.spawnSync = (_command, arguments_) => {
  if (arguments_.includes("build")) return { status: 0, stdout: "", stderr: "" };
  if (arguments_.includes("fuzz")) return fixture;
  throw new Error("unexpected CLI invocation");
};
syncBuiltinESMExports();
`,
      );
      const result = spawnSync(
        process.execPath,
        ["--import", pathToFileURL(preload).href, script, "fuzz-all"],
        {
          encoding: "utf8",
          timeout: commandTimeoutMs,
          killSignal: "SIGKILL",
        },
      );
      assert.ifError(result.error);
      assert.equal(
        result.status,
        fixture.expectedStatus,
        `${fixture.name}\n${result.stdout}${result.stderr}`,
      );
      assert.equal(
        result.stdout,
        fixture.stdout.repeat(
          fixture.expectedStatus === 0 ? grammars.length : 1,
        ),
        `${fixture.name}: CLI stdout differs`,
      );
      assert.equal(
        result.stderr,
        fixture.stderr + (fixture.expectedDiagnostic ?? ""),
        `${fixture.name}: CLI stderr differs`,
      );
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test(`${language}: package metadata matches the grammar and license`, () => {
  const { metadata } = configuration;
  const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  for (const key of ["version", "license", "description"])
    assert.equal(pkg[key], metadata[key]);
  assert.equal(pkg.repository, `git+${metadata.links.repository}.git`);
  assert.ok(
    readFileSync(join(root, "LICENSE"), "utf8").startsWith(
      `${pkg.license} License`,
    ),
  );
});

function run(command, arguments_, cwd = root) {
  const result = spawnSync(command, arguments_, {
    cwd,
    encoding: "utf8",
    timeout: archiveTimeoutMs,
    maxBuffer: archiveOutputLimitBytes,
    killSignal: "SIGKILL",
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return result.stdout;
}

test("npm archives contain reproducible grammars for every language", () => {
  const cache = join(root, "node_modules", ".cache");
  mkdirSync(cache, { recursive: true });
  const directory = mkdtempSync(join(cache, `${packageName}-distribution-`));
  const runner = createTreeSitter();
  try {
    const npm = process.env.npm_execpath;
    assert.ok(npm, "Run distribution checks through npm test.");
    const [archive] = Object.values(
      JSON.parse(
        run(process.execPath, [
          npm,
          "pack",
          "--json",
          "--ignore-scripts",
          "--pack-destination",
          directory,
        ]),
      ),
    );
    const npmRoot = join(directory, "npm");
    mkdirSync(npmRoot);
    run("tar", ["-xf", archive.filename, "-C", "npm"], directory);
    const npmSource = join(npmRoot, "package");
    for (const { name, path } of grammars) {
      const output = join(directory, "generated", name);
      mkdirSync(output, { recursive: true });
      const result = runner.run(
        [
          "generate",
          join(npmSource, path, "grammar.js"),
          "--abi",
          "latest",
          "--output",
          output,
        ],
        { cwd: npmSource },
      );
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stdout + result.stderr);
      for (const file of ["parser.c", "grammar.json", "node-types.json"]) {
        assert.deepEqual(
          readFileSync(join(output, file)),
          readFileSync(join(npmSource, path, "src", file)),
          `${name}: ${file}`,
        );
      }
    }
  } finally {
    runner.close();
    rmSync(directory, { recursive: true, force: true });
  }
});

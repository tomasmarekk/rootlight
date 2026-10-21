import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { copyFiles, packageName, root } from "../../scripts/tree-sitter.js";

const configuration = JSON.parse(
  readFileSync(join(root, "tree-sitter.json"), "utf8"),
);
const grammars = configuration.grammars.map((grammar) => ({
  ...grammar,
  externalFiles: [].concat(grammar["external-files"] ?? []),
  highlights: [].concat(grammar.highlights ?? []),
}));

const language = packageName.slice("tree-sitter-".length).replaceAll("-", "_");

test(`${language}: Rust rebuilds grammars after source or header changes`, () => {
  const directory = mkdtempSync(join(tmpdir(), `${packageName}-build-`));
  const source = join(directory, "source");
  try {
    copyFiles(
      [
        "Cargo.toml",
        "Cargo.lock",
        "bindings/rust",
        "test",
        ...(existsSync(join(root, "common")) ? ["common"] : []),
        ...grammars.flatMap(({ path, externalFiles, highlights }) => [
          join(path, "src"),
          ...externalFiles,
          ...highlights,
        ]),
      ],
      source,
    );

    function check() {
      const result = spawnSync(
        "cargo",
        [
          "check",
          "--locked",
          "--lib",
          "--manifest-path",
          join(source, "Cargo.toml"),
          "--target-dir",
          join(directory, "target"),
        ],
        { encoding: "utf8", timeout: 60_000, killSignal: "SIGKILL" },
      );
      assert.ifError(result.error);
      return { status: result.status, output: result.stdout + result.stderr };
    }

    const initial = check();
    assert.equal(initial.status, 0, initial.output);
    const dependencies = new Set(
      grammars.flatMap(({ path, externalFiles }) => {
        const scanner = join(path, "src", "scanner.c");
        const scannerFiles = [scanner, ...externalFiles].filter((file) =>
          existsSync(join(source, file)),
        );
        const usesAllocator = scannerFiles.some((file) =>
          readFileSync(join(source, file), "utf8").includes(
            "tree_sitter/alloc.h",
          ),
        );
        return [
          join(path, "src", "parser.c"),
          join(path, "src", "tree_sitter", "parser.h"),
          ...(existsSync(join(source, scanner)) ? [scanner] : []),
          ...(usesAllocator
            ? [join(path, "src", "tree_sitter", "alloc.h")]
            : []),
          ...externalFiles.filter((file) => file.endsWith(".h")),
        ];
      }),
    );
    for (const file of dependencies) {
      const path = join(source, file);
      const original = readFileSync(path);
      const marker = "tree_sitter_source_change_requires_rebuild";
      writeFileSync(path, `${original}\n#error ${marker}\n`);
      const changed = check();
      assert.notEqual(changed.status, 0, `${path}: ${changed.output}`);
      assert.ok(changed.output.includes(marker), changed.output);
      writeFileSync(path, original);
      const restored = check();
      assert.equal(restored.status, 0, restored.output);
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

function run(command, arguments_, cwd = root) {
  const result = spawnSync(command, arguments_, {
    cwd,
    encoding: "utf8",
    timeout: 120000,
    maxBuffer: 64 * 1024 * 1024,
    killSignal: "SIGKILL",
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return result.stdout;
}

test("npm and Cargo archives contain buildable bindings for every language", () => {
  const cache = join(root, "node_modules", ".cache");
  mkdirSync(cache, { recursive: true });
  const directory = mkdtempSync(
    join(cache, `${packageName}-rust-distribution-`),
  );
  try {
    const npm = process.env.npm_execpath;
    assert.ok(npm, "Run Rust build checks through npm exec.");
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
    const cargoTarget = join(directory, "cargo-package");
    run("cargo", [
      "package",
      "--locked",
      "--offline",
      "--allow-dirty",
      "--no-verify",
      "--target-dir",
      cargoTarget,
    ]);
    const packageDirectory = join(cargoTarget, "package");
    const archives = readdirSync(packageDirectory).filter((name) =>
      name.endsWith(".crate"),
    );
    assert.equal(archives.length, 1);
    const cargoRoot = join(packageDirectory, "cargo");
    mkdirSync(cargoRoot);
    run("tar", ["-xf", archives[0], "-C", "cargo"], packageDirectory);
    const cargoSource = join(cargoRoot, archives[0].slice(0, -".crate".length));

    for (const source of [npmSource, cargoSource]) {
      copyFiles(["Cargo.lock"], source);
      const output = run("cargo", [
        "test",
        "--locked",
        "--offline",
        "--manifest-path",
        join(source, "Cargo.toml"),
        "--target-dir",
        join(directory, "build"),
        "--test",
        "bindings",
        "languages_parse_gnu_scripts_and_highlight_queries_compile",
        "--",
        "--exact",
      ]);
      assert.match(output, /^test result: ok[.] 1 passed;/m);
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

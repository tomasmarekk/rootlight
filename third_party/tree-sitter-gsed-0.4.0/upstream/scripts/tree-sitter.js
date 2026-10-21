import { spawnSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { symbols, tokens } from "../common/grammar.js";

const root = resolve(import.meta.dirname, "..");
const packageName = JSON.parse(readFileSync(join(root, "package.json"), "utf8"))
  .name.split("/")
  .at(-1);
const configuration = JSON.parse(
  readFileSync(join(root, "tree-sitter.json"), "utf8"),
);
if (
  !Array.isArray(configuration.grammars) ||
  configuration.grammars.length === 0
) {
  throw new Error("tree-sitter.json must define at least one grammar.");
}
for (const grammar of configuration.grammars) {
  for (const field of ["name", "path", "scope"]) {
    if (typeof grammar?.[field] !== "string" || grammar[field].length === 0) {
      throw new Error(
        `tree-sitter.json grammar ${field} must be a non-empty string.`,
      );
    }
  }
}
const grammars = Object.freeze(
  configuration.grammars.map(
    ({ "external-files": externalFiles, highlights, name, path, scope }) =>
      Object.freeze({
        externalFiles: Object.freeze([].concat(externalFiles ?? [])),
        highlights: Object.freeze([].concat(highlights ?? [])),
        name,
        path,
        scope,
      }),
  ),
);
const missingCliMessage = "Tree-sitter CLI is missing; run npm ci.";

function treeSitterExecutable() {
  try {
    return join(
      dirname(
        fileURLToPath(import.meta.resolve("tree-sitter-cli/package.json")),
      ),
      process.platform === "win32" ? "tree-sitter.exe" : "tree-sitter",
    );
  } catch (error) {
    if (
      error instanceof Error &&
      "code" in error &&
      error.code === "ERR_MODULE_NOT_FOUND"
    ) {
      throw new Error(missingCliMessage, { cause: error });
    }
    throw error;
  }
}

function copyFiles(paths, destinationRoot) {
  for (const path of new Set(paths)) {
    const destination = join(destinationRoot, path);
    mkdirSync(dirname(destination), { recursive: true });
    cpSync(join(root, path), destination, { recursive: true });
  }
}

function resultStatus(result) {
  if (result.error) {
    if (result.error.code === "ENOENT") {
      throw new Error(missingCliMessage, { cause: result.error });
    }
    throw result.error;
  }
  if (result.signal) {
    process.stderr.write(`Tree-sitter CLI terminated by ${result.signal}.\n`);
    return 1;
  }
  return result.status ?? 1;
}

function createTreeSitter() {
  const temporaryDirectory = mkdtempSync(join(tmpdir(), `${packageName}-`));
  const cacheDirectory = join(root, "node_modules", ".cache", packageName);
  const configDirectory = join(temporaryDirectory, "config");
  const libraryDirectory = join(temporaryDirectory, "lib");
  const treeSitterConfigDirectory = join(configDirectory, "tree-sitter");
  const parserDirectory = join(temporaryDirectory, "parsers");

  try {
    mkdirSync(cacheDirectory, { recursive: true });
    mkdirSync(libraryDirectory);
    mkdirSync(treeSitterConfigDirectory, { recursive: true });
    copyFiles(
      [
        "tree-sitter.json",
        ...grammars.flatMap((grammar) => [
          join(grammar.path, "src"),
          ...grammar.highlights,
          ...grammar.externalFiles,
        ]),
      ],
      join(parserDirectory, packageName),
    );
    writeFileSync(
      join(treeSitterConfigDirectory, "config.json"),
      `${JSON.stringify({ "parser-directories": [parserDirectory] }, null, 2)}\n`,
    );
  } catch (error) {
    rmSync(temporaryDirectory, { recursive: true, force: true });
    throw error;
  }

  let closed = false;
  return Object.freeze({
    directory: temporaryDirectory,
    configPath: join(treeSitterConfigDirectory, "config.json"),
    close() {
      if (!closed) {
        closed = true;
        rmSync(temporaryDirectory, { recursive: true, force: true });
      }
    },
    run(arguments_, options = {}) {
      if (closed) {
        throw new Error("Tree-sitter runner is closed.");
      }
      return spawnSync(treeSitterExecutable(), arguments_, {
        cwd: root,
        encoding: "utf8",
        maxBuffer: 256 * 1024 * 1024,
        windowsHide: true,
        killSignal: "SIGKILL",
        ...options,
        env: {
          ...process.env,
          ...(process.platform === "darwin"
            ? { CC: process.env.CC ?? "/opt/homebrew/opt/llvm/bin/clang" }
            : {}),
          APPDATA: configDirectory,
          LOCALAPPDATA: cacheDirectory,
          NO_COLOR: "1",
          TREE_SITTER_DIR: treeSitterConfigDirectory,
          TREE_SITTER_LIBDIR: libraryDirectory,
          TREE_SITTER_SEED: process.env.TREE_SITTER_SEED ?? "1",
          XDG_CACHE_HOME: cacheDirectory,
          XDG_CONFIG_HOME: configDirectory,
        },
      });
    },
  });
}

function runChecked(runner, arguments_, options = { stdio: "inherit" }) {
  return resultStatus(runner.run(arguments_, options));
}

function generateParsers(outputRoot = root) {
  mkdirSync(join(outputRoot, "common"), { recursive: true });
  const names = [...tokens.map((name) => name.toUpperCase()), "TOKEN_COUNT"];
  writeFileSync(
    join(outputRoot, "common/tokens.h"),
    `/* Generated by scripts/tree-sitter.js. */\nenum TokenType {\n${names.map((name) => `  ${name}`).join(",\n")}\n};\nstatic inline enum TokenType escaped_symbol(enum TokenType token) {\n  switch (token) {\n${symbols.map((name) => `  case ${name.toUpperCase()}: return ${name.toUpperCase()}_ESCAPED;`).join("\n")}\n  default: return token;\n  }\n}\n`,
  );

  for (const grammar of grammars) {
    const output = join(outputRoot, grammar.path, "src");
    mkdirSync(output, { recursive: true });
    const status = resultStatus(
      spawnSync(
        treeSitterExecutable(),
        [
          "generate",
          join(root, grammar.path, "grammar.js"),
          "--abi",
          "latest",
          "--output",
          output,
        ],
        { cwd: root, windowsHide: true, stdio: "inherit" },
      ),
    );
    if (status !== 0) {
      return status;
    }
  }
  return 0;
}

function buildParsers(runner) {
  const directory = join(root, "build");
  mkdirSync(directory, { recursive: true });
  for (const { name, path } of grammars) {
    const library = join(
      directory,
      `${name}.${process.platform === "win32" ? "dll" : "so"}`,
    );
    const status = runChecked(runner, [
      "build",
      join(root, path),
      "--output",
      library,
    ]);
    if (status !== 0) return status;
  }
  return 0;
}

function testCorpus(arguments_) {
  if (
    arguments_.some(
      (argument) =>
        ["--update", "--debug-graph", "--open-log"].includes(argument) ||
        /^-[d0rh]*[uD]/.test(argument),
    )
  ) {
    throw new Error(
      "test-corpus deletes its isolated copy; --update, --debug-graph, and --open-log would lose their output.",
    );
  }
  const testRoot = mkdtempSync(join(tmpdir(), `${packageName}-test-`));
  let runner;

  try {
    copyFiles(
      [
        "package.json",
        ...(existsSync(join(root, "common")) ? ["common"] : []),
        ...grammars.flatMap(({ path, externalFiles, highlights }) => [
          join(path, "grammar.js"),
          join(path, "src"),
          ...externalFiles,
          ...highlights,
        ]),
        join("test", "corpus"),
        "tree-sitter.json",
      ],
      testRoot,
    );
    runner = createTreeSitter();
    return runChecked(runner, ["test", ...arguments_], {
      cwd: testRoot,
      stdio: "inherit",
    });
  } finally {
    try {
      runner?.close();
    } finally {
      rmSync(testRoot, { recursive: true, force: true });
    }
  }
}

function fuzzParsers(runner, arguments_) {
  const directory = mkdtempSync(join(tmpdir(), `${packageName}-fuzz-`));
  try {
    for (const { name, path } of grammars) {
      const library = join(
        directory,
        `${name}.${process.platform === "win32" ? "dll" : "so"}`,
      );
      const buildStatus = runChecked(runner, [
        "build",
        join(root, path),
        "--output",
        library,
      ]);
      if (buildStatus !== 0) return buildStatus;
      const result = runner.run(
        ["fuzz", "--lib-path", library, "--lang-name", name, ...arguments_],
        {
          encoding: "utf8",
          maxBuffer: 16 * 1024 * 1024,
          timeout: 600_000,
          killSignal: "SIGKILL",
        },
      );
      process.stdout.write(result.stdout ?? "");
      process.stderr.write(result.stderr ?? "");
      const status = resultStatus(result);
      if (status !== 0) return status;
      if (
        /^[1-9][0-9]* .+ corpus tests failed fuzzing$/m.test(
          result.stdout + result.stderr,
        )
      )
        return 1;
    }
    return 0;
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

function checkQueries() {
  const configuration = JSON.parse(
    readFileSync(join(root, ".tsqueryrc.json"), "utf8"),
  );
  for (const grammar of grammars) {
    const parserAliases = { ...configuration.parser_aliases };
    for (const { name } of grammars) {
      if (name === grammar.name) delete parserAliases[name];
      else parserAliases[name] = grammar.name;
    }
    const directories = [...new Set(grammar.highlights.map(dirname))];
    const settings = { ...configuration, parser_aliases: parserAliases };
    const result = spawnSync(
      "ts_query_ls",
      [
        "check",
        "--format",
        "--config",
        JSON.stringify(settings),
        ...directories,
      ],
      {
        cwd: root,
        encoding: "utf8",
        timeout: 60_000,
        killSignal: "SIGKILL",
      },
    );
    process.stdout.write(result.stdout ?? "");
    process.stderr.write(result.stderr ?? "");
    if (result.error) throw result.error;
    if (result.signal) {
      process.stderr.write(`ts_query_ls terminated by ${result.signal}.\n`);
      return 1;
    }
    if (result.status !== 0) return result.status ?? 1;
    console.log(`${grammar.name}: reference queries passed`);
  }
  return 0;
}

function main(arguments_) {
  const [command, ...rest] = arguments_;
  if (command === "generate-all") {
    if (rest.length !== 0) {
      throw new Error("Usage: node scripts/tree-sitter.js generate-all");
    }
    return generateParsers();
  }
  if (command === "check-queries") {
    if (rest.length !== 0) {
      throw new Error("Usage: node scripts/tree-sitter.js check-queries");
    }
    return checkQueries();
  }
  if (command === "test-corpus") {
    return testCorpus(rest);
  }

  const runner = createTreeSitter();
  try {
    if (command === "build-all") {
      if (rest.length !== 0) {
        throw new Error("Usage: node scripts/tree-sitter.js build-all");
      }
      return buildParsers(runner);
    }
    if (command === "fuzz-all") return fuzzParsers(runner, rest);
    return runChecked(runner, arguments_);
  } finally {
    runner.close();
  }
}

if (import.meta.main === undefined) {
  throw new Error("Node.js 24.21.0 or later is required.");
}

if (import.meta.main) {
  try {
    process.exitCode = main(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

export {
  copyFiles,
  createTreeSitter,
  generateParsers,
  grammars,
  packageName,
  root,
};

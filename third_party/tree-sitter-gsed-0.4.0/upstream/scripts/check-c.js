import { spawnSync } from "node:child_process";
import {
  accessSync,
  constants,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, delimiter, dirname, isAbsolute, join } from "node:path";
import { grammars, packageName, root } from "./tree-sitter.js";

const enumerators = {};
const tokenCount = "TOKEN_COUNT";

const scannerDiagnosticArguments = [];

const warningArguments = ["-Wall", "-Wextra", "-Werror", "-pedantic"];
const scannerContract = join(root, "test", "scanner.test.c");
const contracts = [scannerContract];

const variants = grammars.map((grammar) => {
  const includeDirectory = join(root, grammar.path, "src");
  const source = join(includeDirectory, "scanner.c");
  const headers = grammar.externalFiles
    .filter((file) => file.endsWith(".h") && file !== "common/tokens.h")
    .map((file) => join(root, file));
  return {
    name: grammar.name,
    includeDirectory,
    source,
    headers,
    contractArguments: [
      `-DGSED_EXTENDED=${grammar.name === "gsed_ere" ? 1 : 0}`,
    ],
    reuseAllocator: [source, ...headers].some((file) =>
      readFileSync(file, "utf8").includes('"tree_sitter/alloc.h"'),
    ),
  };
});

function run(command, arguments_, options = {}) {
  const result = spawnSync(command, arguments_, {
    cwd: root,
    encoding: "utf8",
    timeout: 60_000,
    killSignal: "SIGKILL",
    maxBuffer: 64 * 1024 * 1024,
    stdio: "inherit",
    ...options,
  });
  if (result.error) {
    throw new Error(
      `${command} ${arguments_.join(" ")}: ${result.error.message}`,
      { cause: result.error },
    );
  }
  if (result.status !== 0) {
    const diagnostics = (result.stderr || result.stdout || "").trim();
    const outcome = result.signal
      ? `terminated by ${result.signal}`
      : `failed with status ${result.status}`;
    throw new Error(
      `${command} ${arguments_.join(" ")} ${outcome}${diagnostics ? `\n${diagnostics}` : ""}`,
    );
  }
  return result;
}

function executableCandidates(name) {
  if (process.platform !== "win32") return [name];
  const extensions = (process.env.PATHEXT ?? ".EXE;.CMD;.BAT")
    .split(";")
    .filter(Boolean)
    .map((extension) => extension.toLowerCase());
  if (extensions.some((extension) => name.toLowerCase().endsWith(extension)))
    return [name];
  return [name, ...extensions.map((extension) => name + extension)];
}

function isExecutable(path) {
  try {
    accessSync(path, constants.X_OK);
    return statSync(path).isFile();
  } catch {
    return false;
  }
}

function findExecutable(name, directories) {
  if (isAbsolute(name) || name.includes("/") || name.includes("\\")) {
    if (isExecutable(name)) return name;
    throw new Error(`Cannot execute ${name}.`);
  }
  for (const directory of directories) {
    for (const candidate of executableCandidates(name)) {
      const path = join(directory, candidate);
      if (isExecutable(path)) return path;
    }
  }
  throw new Error(`Cannot find ${name} on PATH.`);
}

function versionMajor(command) {
  const version = run(command, ["--version"], { stdio: "pipe" }).stdout;
  const match = version.match(/version ([0-9]+)/);
  if (match === null)
    throw new Error(`Cannot determine the version of ${command}.`);
  return Number(match[1]);
}

function llvmCommands() {
  if (process.platform === "darwin") {
    const directory = "/opt/homebrew/opt/llvm/bin";
    return {
      clang: findExecutable(join(directory, "clang"), []),
      clangd: findExecutable(join(directory, "clangd"), []),
      clangFormat: findExecutable(join(directory, "clang-format"), []),
    };
  }
  const directories = (process.env.PATH ?? "").split(delimiter);
  const clangFormat = findExecutable(
    process.env.CLANG_FORMAT ?? "clang-format",
    directories,
  );
  const directory = dirname(realpathSync(clangFormat));
  return {
    clang: findExecutable(
      process.env.CLANG ?? "clang",
      process.env.CLANG === undefined ? [directory] : directories,
    ),
    clangd: findExecutable(
      process.env.CLANGD ?? "clangd",
      process.env.CLANGD === undefined ? [directory] : directories,
    ),
    clangFormat,
  };
}

function checkExternalTokenOrder(clang, compilerArguments, variant, directory) {
  const grammar = JSON.parse(
    readFileSync(join(variant.includeDirectory, "grammar.json"), "utf8"),
  );
  const assertions = grammar.externals.map((external, index) => {
    const token =
      external.type === "IMMEDIATE_TOKEN" ? external.content : external;
    const name = token.type === "SYMBOL" ? token.name : token.value;
    const enumerator =
      enumerators[name] ??
      (token.type === "SYMBOL"
        ? name.replace(/^_/, "").toUpperCase()
        : undefined);
    if (enumerator === undefined) {
      throw new Error(
        `Missing scanner enumerator for ${JSON.stringify(external)}.`,
      );
    }
    return `typedef char external_${index}[${enumerator} == ${index} ? 1 : -1];`;
  });
  assertions.push(
    `typedef char external_count[${tokenCount} == ${grammar.externals.length} ? 1 : -1];`,
  );
  const source = join(directory, `scanner-indices-${variant.name}.c`);
  writeFileSync(
    source,
    `#include ${JSON.stringify(variant.source.replaceAll("\\", "/"))}\n${assertions.join("\n")}\n`,
  );
  run(clang, [...compilerArguments, "-fsyntax-only", source]);
}

function checkDiagnostics(clang, clangd, directory) {
  for (const variant of variants) {
    const databaseDirectory = join(directory, variant.name);
    mkdirSync(databaseDirectory);
    const sources = [variant.source, ...variant.headers, ...contracts];
    const commands = sources.map((source) => ({
      arguments: [
        clang,
        "-std=c17",
        source.endsWith(".h") ? "-xc-header" : "-xc",
        "-I",
        variant.includeDirectory,
        ...warningArguments,
        // Standalone checks omit some callers.
        ...(source === variant.source
          ? scannerDiagnosticArguments
          : ["-Wno-unused-function", ...variant.contractArguments]),
        "-fsyntax-only",
        source,
      ],
      directory: root,
      file: source,
    }));
    writeFileSync(
      join(databaseDirectory, "compile_commands.json"),
      `${JSON.stringify(commands)}\n`,
    );
    for (const source of sources) {
      process.stdout.write(`clangd: ${variant.name}: ${source}\n`);
      run(
        clangd,
        [
          "--enable-config=false",
          "--log=error",
          "--tweaks=",
          `--compile-commands-dir=${databaseDirectory}`,
          `--check=${source}`,
        ],
        { timeout: 180_000 },
      );
    }
  }
}

function checkAllocatorSymbols(clang, variant, directory) {
  const object = join(directory, `scanner-reuse-${variant.name}.o`);
  run(clang, [
    "-std=c17",
    ...warningArguments,
    "-I",
    variant.includeDirectory,
    "-DTREE_SITTER_REUSE_ALLOCATOR",
    "-c",
    variant.source,
    "-o",
    object,
  ]);
  const symbols = new Set(
    run("nm", ["-u", object], { stdio: "pipe" })
      .stdout.split("\n")
      .map((line) => line.trim().split(/ +/).at(-1)?.replace(/^_/, "")),
  );
  const allocators = ["malloc", "calloc", "realloc", "free"];
  if (allocators.some((allocator) => symbols.has(allocator)))
    throw new Error("Scanner bypasses the Tree-sitter allocator.");
  if (!allocators.some((allocator) => symbols.has(`ts_current_${allocator}`)))
    throw new Error("Scanner does not reference the Tree-sitter allocator.");
}

function main(arguments_) {
  if (
    arguments_.length > 1 ||
    (arguments_.length === 1 &&
      !["--write", "--sanitize"].includes(arguments_[0]))
  ) {
    throw new Error("Usage: node scripts/check-c.js [--write | --sanitize]");
  }
  const sources = [
    ...new Set([
      ...variants.flatMap((variant) => variant.headers),
      ...variants.map((variant) => variant.source),
      ...contracts,
    ]),
  ];
  const { clang, clangd, clangFormat } = llvmCommands();
  if (new Set([clang, clangd, clangFormat].map(versionMajor)).size !== 1)
    throw new Error(
      "clang, clangd, and clang-format must use the same LLVM major version.",
    );

  if (arguments_[0] === "--write") {
    run(clangFormat, ["-i", ...sources]);
    return;
  }
  const sanitizerArguments =
    arguments_[0] === "--sanitize"
      ? [
          "-fsanitize=address,undefined",
          "-fno-sanitize-recover=undefined",
          "-fno-omit-frame-pointer",
          "-g",
        ]
      : [];
  const directory = mkdtempSync(join(tmpdir(), `${packageName}-scanner-`));
  try {
    checkDiagnostics(clang, clangd, directory);
    run(clangFormat, ["--dry-run", "--Werror", ...sources]);
    for (const standard of ["c99", "c17"]) {
      for (const variant of variants) {
        const compilerArguments = [
          ...sanitizerArguments,
          `-std=${standard}`,
          ...warningArguments,
          "-I",
          variant.includeDirectory,
        ];
        checkExternalTokenOrder(clang, compilerArguments, variant, directory);
        for (const contract of contracts) {
          const modes =
            contract === scannerContract && variant.reuseAllocator
              ? [false, true]
              : [false];
          for (const reuse of modes) {
            const suffix = process.platform === "win32" ? ".exe" : "";
            const contractName = basename(contract, ".test.c");
            const name = `${variant.name}-${contractName}-${standard}${reuse ? "-reuse" : ""}`;
            const binary = join(directory, `scanner-${name}${suffix}`);
            run(clang, [
              ...compilerArguments,
              ...variant.contractArguments,
              ...(reuse ? ["-DTREE_SITTER_REUSE_ALLOCATOR"] : []),
              contract,
              "-o",
              binary,
            ]);
            run(binary, []);
            process.stdout.write(
              `${variant.name}: ${contractName} tests passed (${standard.toUpperCase()}${reuse ? ", reused allocator" : ""})\n`,
            );
          }
        }
        if (standard === "c17" && variant.reuseAllocator)
          checkAllocatorSymbols(clang, variant, directory);
      }
    }
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
}

try {
  main(process.argv.slice(2));
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}

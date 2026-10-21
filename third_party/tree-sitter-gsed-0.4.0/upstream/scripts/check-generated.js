import {
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";
import { generateParsers, grammars, packageName, root } from "./tree-sitter.js";

const generatedPaths = [
  "grammar.json",
  "node-types.json",
  "parser.c",
  join("tree_sitter", "alloc.h"),
  join("tree_sitter", "array.h"),
  join("tree_sitter", "parser.h"),
].sort();

function listFiles(directory, prefix = "") {
  const paths = [];
  for (const entry of readdirSync(join(directory, prefix), {
    withFileTypes: true,
  })) {
    const path = join(prefix, entry.name);
    if (entry.isDirectory()) {
      paths.push(...listFiles(directory, path));
    } else {
      paths.push(path);
    }
  }
  return paths.sort();
}

function different(left, right) {
  try {
    return !readFileSync(left).equals(readFileSync(right));
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT")
      return true;
    throw error;
  }
}

function readDefinition(parser, name, path) {
  const match = parser.match(new RegExp(`^#define ${name} ([0-9]+)$`, "m"));
  if (match === null) {
    throw new Error(`${path} does not define ${name} as an integer`);
  }
  return Number(match[1]);
}

function maximumActionIndex(parser, path) {
  let maximum;
  for (const match of parser.matchAll(/ACTIONS\(([0-9]+)\)/g)) {
    const value = Number(match[1]);
    maximum = maximum === undefined ? value : Math.max(maximum, value);
  }
  if (maximum === undefined) {
    throw new Error(`${path} contains no ACTIONS index`);
  }
  return maximum;
}

function smallParseTableWordCount(parser, path) {
  const declaration = "static const uint16_t ts_small_parse_table[] = {\n";
  const start = parser.indexOf(declaration);
  if (start === -1) {
    throw new Error(`${path} contains no small parse table`);
  }
  const initializerStart = start + declaration.length;
  const initializerEnd = parser.indexOf("\n};", initializerStart);
  if (initializerEnd === -1) {
    throw new Error(`${path} contains an unterminated small parse table`);
  }
  const initializer = parser.slice(initializerStart, initializerEnd);

  let finalIndex;
  let finalOffset;
  for (const match of initializer.matchAll(/^ {2}\[([0-9]+)\] =/gm)) {
    finalIndex = Number(match[1]);
    finalOffset = match.index;
  }
  if (finalIndex === undefined || finalOffset === undefined) {
    throw new Error(`${path} small parse table has no indexed row`);
  }

  const finalRowWordCount =
    initializer.slice(finalOffset).match(/,/g)?.length ?? 0;
  if (finalRowWordCount === 0) {
    throw new Error(`${path} small parse table has an empty final row`);
  }
  return finalIndex + finalRowWordCount;
}

function parseTableStorageBytes(parser, metrics, path) {
  const smallStateCount = metrics.STATE_COUNT - metrics.LARGE_STATE_COUNT;
  if (smallStateCount < 0) {
    throw new Error(`${path} has more large states than total states`);
  }
  return (
    metrics.LARGE_STATE_COUNT * metrics.SYMBOL_COUNT * 2 +
    smallParseTableWordCount(parser, path) * 2 +
    smallStateCount * 4
  );
}

function checkParser(grammar, generatedRoot) {
  const parserPath = join(generatedRoot, grammar.path, "src", "parser.c");
  const displayPath = relative(generatedRoot, parserPath);
  const parser = readFileSync(parserPath, "utf8");
  const metrics = {
    LANGUAGE_VERSION: readDefinition(parser, "LANGUAGE_VERSION", displayPath),
    STATE_COUNT: readDefinition(parser, "STATE_COUNT", displayPath),
    LARGE_STATE_COUNT: readDefinition(parser, "LARGE_STATE_COUNT", displayPath),
    SYMBOL_COUNT: readDefinition(parser, "SYMBOL_COUNT", displayPath),
    EXTERNAL_TOKEN_COUNT: readDefinition(
      parser,
      "EXTERNAL_TOKEN_COUNT",
      displayPath,
    ),
    parser_bytes: statSync(parserPath).size,
    maximum_ACTIONS_index: maximumActionIndex(parser, displayPath),
  };
  metrics.parse_table_storage_bytes = parseTableStorageBytes(
    parser,
    metrics,
    displayPath,
  );

  console.log(`${grammar.name}:`);
  console.log("Metric                           Actual");
  for (const [name, value] of Object.entries(metrics)) {
    console.log(`${name.padEnd(28)} ${String(value).padStart(12)}`);
  }
  return metrics.LANGUAGE_VERSION;
}

function main(arguments_) {
  if (arguments_.length !== 0) {
    throw new Error("Usage: node scripts/check-generated.js");
  }

  const generatedRoot = mkdtempSync(
    join(tmpdir(), `${packageName}-generated-`),
  );
  try {
    if (generateParsers(generatedRoot) !== 0) return 1;

    let failed = false;
    const stale = [];
    if (
      different(
        join(generatedRoot, "common/tokens.h"),
        join(root, "common/tokens.h"),
      )
    )
      stale.push("common/tokens.h");
    for (const grammar of grammars) {
      const generatedDirectory = join(generatedRoot, grammar.path, "src");
      const actualPaths = listFiles(generatedDirectory);
      if (JSON.stringify(actualPaths) !== JSON.stringify(generatedPaths)) {
        console.error(`${grammar.name}: generated file manifest differs.`);
        console.error(`Expected:\n${generatedPaths.join("\n")}`);
        console.error(`Actual:\n${actualPaths.join("\n")}`);
        failed = true;
      }
      for (const path of generatedPaths) {
        const generatedPath = join(generatedDirectory, path);
        const repositoryPath = join(root, grammar.path, "src", path);
        if (different(generatedPath, repositoryPath)) {
          stale.push(relative(root, repositoryPath));
        }
      }
    }
    if (stale.length > 0) {
      console.error("Generated parser files are stale or missing:");
      for (const path of stale) console.error(`  ${path}`);
      console.error("Run npm run generate and review the results.");
      failed = true;
    }

    const languageVersions = new Map();
    for (const grammar of grammars) {
      languageVersions.set(grammar.name, checkParser(grammar, generatedRoot));
    }
    if (new Set(languageVersions.values()).size !== 1) {
      console.error(
        "Generated parsers use different Tree-sitter ABI versions:",
      );
      for (const [name, version] of languageVersions) {
        console.error(`  ${name}: ${version}`);
      }
      failed = true;
    }
    return failed ? 1 : 0;
  } finally {
    rmSync(generatedRoot, { recursive: true, force: true });
  }
}

try {
  process.exitCode = main(process.argv.slice(2));
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}

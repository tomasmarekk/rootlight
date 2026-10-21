import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before } from "node:test";
import { createTreeSitter } from "../../scripts/tree-sitter.js";
import {
  issueSignatures,
  parseCst,
  publicNodes,
  sourceEndPoint,
} from "./cst.js";

const commandTimeoutMs = 60_000;
const cstOutputLimitBytes = 16 * 1024 * 1024;
const summaryTimeoutMicros = 10_000_000;

let temporaryDirectory;
let treeSitter;
let fixtureNumber = 0;

before(() => {
  temporaryDirectory = mkdtempSync(join(tmpdir(), "tree-sitter-gsed-parser-"));
  try {
    treeSitter = createTreeSitter();
  } catch (error) {
    rmSync(temporaryDirectory, { recursive: true, force: true });
    throw error;
  }
});

after(() => {
  try {
    treeSitter?.close();
  } finally {
    if (temporaryDirectory !== undefined) {
      rmSync(temporaryDirectory, { recursive: true, force: true });
    }
  }
});

function applyEdits(source, edits) {
  let bytes = Buffer.from(source);
  for (const edit of edits) {
    const { byte, deleteBytes, insert } = edit;
    const description = JSON.stringify(edit);
    assert.ok(
      Number.isSafeInteger(byte) && byte >= 0,
      `invalid byte offset: ${description}`,
    );
    assert.ok(
      Number.isSafeInteger(deleteBytes) && deleteBytes >= 0,
      `invalid deletion length: ${description}`,
    );
    assert.equal(typeof insert, "string", `invalid insertion: ${description}`);
    assert.ok(
      byte <= bytes.length && deleteBytes <= bytes.length - byte,
      `edit exceeds ${bytes.length} source bytes: ${description}`,
    );
    bytes = Buffer.concat([
      bytes.subarray(0, byte),
      Buffer.from(insert),
      bytes.subarray(byte + deleteBytes),
    ]);
  }
  return bytes;
}

function formatEdit({ byte, deleteBytes, insert }) {
  return `${byte} ${deleteBytes} ${insert}`;
}

function parse(scope, source, edits = []) {
  const sourcePath = join(temporaryDirectory, `fixture-${fixtureNumber}.sed`);
  fixtureNumber += 1;
  writeFileSync(sourcePath, source);
  const result = treeSitter.run(
    [
      "parse",
      "--encoding",
      "utf8",
      "--scope",
      scope,
      "--cst",
      sourcePath,
      ...(edits.length === 0 ? [] : ["--edits", ...edits.map(formatEdit)]),
    ],
    {
      encoding: "utf8",
      maxBuffer: cstOutputLimitBytes,
      timeout: commandTimeoutMs,
    },
  );
  if (result.error) {
    throw result.error;
  }
  assert.ok(
    result.status === 0 || result.status === 1,
    result.stdout + result.stderr,
  );
  result.nodes = parseCst(result.stdout, applyEdits(source, edits));
  return result;
}

function parseSuccessfully(scope, source, edits = []) {
  const result = parse(scope, source, edits);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.doesNotMatch(result.stdout, /^[0-9: \t-]+•/m, result.stdout);
  return result;
}

function assertIncrementalContract(fresh, incremental, context) {
  const freshIssues = issueSignatures(fresh.nodes);
  assert.deepEqual(
    issueSignatures(incremental.nodes),
    freshIssues,
    `${context}: fresh and incremental issue signatures differ`,
  );
  assert.deepEqual(
    publicNodes(incremental.nodes),
    publicNodes(fresh.nodes),
    `${context}: fresh and incremental public CSTs differ`,
  );
  if (fresh.status === 0 && freshIssues.length === 0) {
    assert.equal(incremental.status, 0, `${context}: normal source has errors`);
    assert.deepEqual(
      incremental.nodes,
      fresh.nodes,
      `${context}: fresh and incremental CSTs differ`,
    );
  }
}

function parseSummary(scope, source, timeoutMicros = summaryTimeoutMicros) {
  const sourcePath = join(temporaryDirectory, `summary-${fixtureNumber++}.sed`);
  writeFileSync(sourcePath, source);
  const result = treeSitter.run(
    [
      "parse",
      "--encoding",
      "utf8",
      "--scope",
      scope,
      "--quiet",
      "--json-summary",
      "--timeout",
      String(timeoutMicros),
      sourcePath,
    ],
    { encoding: "utf8", timeout: commandTimeoutMs },
  );
  assert.ifError(result.error);
  assert.ok(
    result.status === 0 || result.status === 1,
    result.stdout + result.stderr,
  );
  const start = result.stdout.indexOf("{\n");
  assert.notEqual(start, -1, "parser produced no summary");
  const { parse_summaries: summaries } = JSON.parse(result.stdout.slice(start));
  assert.equal(summaries?.length, 1, "parser produced no complete root");
  const summary = summaries[0];
  assert.equal(
    `${summary.end.row}:${summary.end.column}`,
    sourceEndPoint(source),
    "root must reach the source end",
  );
  assert.equal(
    summary.bytes,
    Buffer.byteLength(source),
    "parser omitted source bytes",
  );
  return summary;
}

export {
  applyEdits,
  assertIncrementalContract,
  parse,
  parseSuccessfully,
  parseSummary,
};

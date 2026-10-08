import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { compactGeneratedC } from "./compact_generated_c.mjs";

test("only consecutive line-prefix ASCII spaces are removed", () => {
  const input = Buffer.from('  #define VALUE 7\n    const char *s = "  kept  ";  \n \n  \t  VALUE,\n');
  const saved = Buffer.from(input);
  const result = compactGeneratedC(input);
  assert.deepEqual(result.output, Buffer.from('#define VALUE 7\nconst char *s = "  kept  ";  \n\n\t  VALUE,\n'));
  assert.equal(result.removed, 9);
  assert.equal(result.lines, 4);
  assert.deepEqual(input, saved);
  assert.deepEqual(compactGeneratedC(result.output).output, result.output);
});

test("invalid physical-line or byte inputs are rejected", () => {
  for (const input of [Buffer.alloc(0), Buffer.from("int x;"), Buffer.from("int x;\r\n"),
    Buffer.from([0, 10]), Buffer.from("é\n"), Buffer.from("\\\n  continued\n"),
    Buffer.from("??/\n  continued\n")]) {
    assert.throws(() => compactGeneratedC(input));
  }
  assert.throws(() => compactGeneratedC("not a buffer"));
});

test("ordinary slash, question marks and final tabs remain literal bytes", () => {
  const input = Buffer.from('  /* text ??/ is not a splice here */\n  \\t\n\t\n');
  assert.deepEqual(compactGeneratedC(input).output, Buffer.from('/* text ??/ is not a splice here */\n\\t\n\t\n'));
});

test("CLI creates a fresh output and does not overwrite any existing file", () => {
  const directory = mkdtempSync(join(tmpdir(), "generated-c-"));
  try {
    const inputPath = join(directory, "input.c");
    const outputPath = join(directory, "output.c");
    const input = Buffer.from("  int value = 7;\n");
    writeFileSync(inputPath, input);
    const invoke = output => spawnSync(process.execPath, [
      fileURLToPath(new URL("./compact_generated_c.mjs", import.meta.url)), inputPath, output,
    ], { encoding: "utf8" });
    const first = invoke(outputPath);
    assert.equal(first.status, 0, first.stderr);
    const receipt = JSON.parse(first.stdout);
    assert.equal(receipt.removed_ascii_spaces, 2);
    assert.equal(receipt.lf_count, 1);
    assert.deepEqual(readFileSync(outputPath), Buffer.from("int value = 7;\n"));
    assert.notEqual(invoke(outputPath).status, 0);
    assert.notEqual(invoke(inputPath).status, 0);
    assert.deepEqual(readFileSync(inputPath), input);
    assert.deepEqual(readFileSync(outputPath), Buffer.from("int value = 7;\n"));
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

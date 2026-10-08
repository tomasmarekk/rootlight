// Keep generated parser tables reproducible while reducing line-prefix whitespace.
// This bounded byte transform is not a general-purpose C formatter.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Remove only consecutive ASCII spaces at the start of physical generated lines.
 *
 * The input is conserved, and a second line-by-line check verifies every retained
 * byte. Reject physical-line splices because their prefixes can belong to literals.
 *
 * @param {Buffer} input Nonempty ASCII TAB/LF bytes terminated by LF.
 * @returns {{ output: Buffer, removed: number, lines: number }} Verified output and counts.
 * @throws {Error} If the input is outside the supported subset or retained bytes differ.
 */
export function compactGeneratedC(input) {
  if (!Buffer.isBuffer(input) || input.length === 0 || input.at(-1) !== 10) {
    throw new Error("generated C requires a nonempty buffer with final LF");
  }
  let lines = 0;
  for (let i = 0; i < input.length; i++) {
    const byte = input[i];
    if (byte !== 9 && byte !== 10 && (byte < 32 || byte > 126)) {
      throw new Error("generated C is outside the ASCII TAB/LF subset");
    }
    // A spliced physical line can make its leading spaces part of a literal.
    if (byte === 10 && (input[i - 1] === 92 ||
        (input[i - 1] === 47 && input[i - 2] === 63 && input[i - 3] === 63))) {
      throw new Error("C physical-line continuation is outside the transform subset");
    }
    if (byte === 10) lines++;
  }

  const storage = Buffer.allocUnsafe(input.length);
  let written = 0;
  let lineStart = true;
  for (const byte of input) {
    if (lineStart && byte === 32) continue;
    storage[written++] = byte;
    lineStart = byte === 10;
  }
  const output = storage.subarray(0, written);

  // Independently compare complete physical lines, not the transform's flags.
  let inputStart = 0;
  let outputStart = 0;
  let removed = 0;
  while (inputStart < input.length) {
    const inputEnd = input.indexOf(10, inputStart) + 1;
    let keptStart = inputStart;
    while (input[keptStart] === 32) keptStart++;
    const length = inputEnd - keptStart;
    if (!input.subarray(keptStart, inputEnd).equals(output.subarray(outputStart, outputStart + length))) {
      throw new Error("non-prefix byte changed");
    }
    removed += keptStart - inputStart;
    inputStart = inputEnd;
    outputStart += length;
  }
  if (outputStart !== output.length) throw new Error("extra output bytes");
  return { output, removed, lines };
}

function main() {
  if (process.argv.length !== 4) {
    throw new Error("usage: node scripts/compact_generated_c.mjs INPUT OUTPUT");
  }
  const input = readFileSync(process.argv[2]);
  const result = compactGeneratedC(input);
  // Never overwrite the canonical generator output or an existing artifact.
  writeFileSync(process.argv[3], result.output, { flag: "wx" });
  if (!readFileSync(process.argv[3]).equals(result.output)) {
    throw new Error("written artifact differs from verified output");
  }
  const sha256 = bytes => createHash("sha256").update(bytes).digest("hex");
  process.stdout.write(JSON.stringify({
    transform: "remove-only-consecutive-line-prefix-ascii-space-0x20",
    input_bytes: input.length,
    input_sha256: sha256(input),
    output_bytes: result.output.length,
    output_sha256: sha256(result.output),
    removed_ascii_spaces: result.removed,
    lf_count: result.lines,
  }) + "\n");
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();

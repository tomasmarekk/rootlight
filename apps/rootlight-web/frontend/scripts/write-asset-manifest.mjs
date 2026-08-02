// Produces the bounded, stable asset inventory verified by the Rust web host.

import { createHash } from "node:crypto";
import { lstat, readdir, readFile, writeFile } from "node:fs/promises";
import { relative, resolve, sep } from "node:path";

const distributionRoot = resolve(import.meta.dirname, "..", "dist");
const manifestName = "asset-manifest.json";
const maximumAssets = 1_024;
const maximumAssetBytes = 16 * 1024 * 1024;
const maximumTotalBytes = 64 * 1024 * 1024;

async function collectFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries.sort((left, right) => compareText(left.name, right.name))) {
    const path = resolve(directory, entry.name);
    if (entry.isSymbolicLink()) {
      throw new Error("asset tree contains a symbolic link");
    }
    if (entry.isDirectory()) {
      files.push(...(await collectFiles(path)));
    } else if (entry.isFile() && entry.name !== manifestName) {
      files.push(path);
    } else if (!entry.isFile()) {
      throw new Error("asset tree contains an unsupported file type");
    }
  }
  return files;
}

const files = await collectFiles(distributionRoot);
if (files.length === 0 || files.length > maximumAssets) {
  throw new Error("asset count is outside the supported range");
}

let totalBytes = 0;
const assets = [];
for (const file of files) {
  const metadata = await lstat(file);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > maximumAssetBytes) {
    throw new Error("asset file is unsupported or oversized");
  }
  totalBytes += metadata.size;
  if (totalBytes > maximumTotalBytes) {
    throw new Error("asset inventory is oversized");
  }
  const bytes = await readFile(file);
  assets.push({
    path: relative(distributionRoot, file).split(sep).join("/"),
    bytes: metadata.size,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  });
}

assets.sort((left, right) => compareText(left.path, right.path));
await writeFile(
  resolve(distributionRoot, manifestName),
  `${JSON.stringify({ schema_version: 1, assets }, null, 2)}\n`,
  { encoding: "utf8", flag: "w" },
);

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

// Builds the exact third-party notice bundle shipped beside the production UI.
//
// Package identities come from package-lock.json; license text is copied only
// from the matching installed package so the release never invents obligations.

import { lstat, readdir, readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const outputFlag = process.argv.indexOf("--output");
if (outputFlag === -1 || outputFlag + 1 >= process.argv.length) {
  throw new Error("usage: write-third-party-notices.mjs --output PATH");
}
const output = resolve(process.argv[outputFlag + 1]);
const lock = JSON.parse(await readFile(resolve(root, "package-lock.json"), "utf8"));
if (lock.lockfileVersion !== 3 || typeof lock.packages !== "object") {
  throw new Error("package lock does not use the supported schema");
}

const records = [];
for (const [packagePath, record] of Object.entries(lock.packages)) {
  if (packagePath === "" || record.dev === true) {
    continue;
  }
  if (
    typeof record.version !== "string" ||
    typeof record.license !== "string" ||
    record.license.length === 0
  ) {
    throw new Error("production package lacks a declared version or license");
  }
  const directory = resolve(root, packagePath);
  const manifest = JSON.parse(await readFile(resolve(directory, "package.json"), "utf8"));
  if (
    typeof manifest.name !== "string" ||
    manifest.version !== record.version ||
    manifest.license !== record.license
  ) {
    throw new Error("installed package identity differs from package-lock.json");
  }
  const candidates = (await readdir(directory))
    .filter((name) => /^(?:licen[cs]e|copying|notice)(?:[._-].*)?$/iu.test(name))
    .sort(compareText);
  const licenseFiles = [];
  for (const name of candidates) {
    const path = resolve(directory, name);
    const metadata = await lstat(path);
    if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > 1024 * 1024) {
      throw new Error(`production package ${manifest.name} has an invalid license file`);
    }
    const text = await readFile(path, "utf8");
    licenseFiles.push({ name: basename(path), text: normalizeText(text) });
  }
  if (licenseFiles.length === 0) {
    licenseFiles.push({
      name: "package.json license declaration",
      text:
        `The exact ${manifest.name} ${manifest.version} package declares ` +
        `${record.license} and does not carry a standalone license file. ` +
        "No replacement copyright notice or license text has been inferred.",
    });
  }
  records.push({
    license: record.license,
    licenseFiles,
    name: manifest.name,
    packagePath,
    version: record.version,
  });
}

records.sort((left, right) => {
  const identity = compareText(left.name, right.name);
  return identity === 0
    ? compareText(left.version, right.version) || compareText(left.packagePath, right.packagePath)
    : identity;
});
if (records.length === 0) {
  throw new Error("production dependency notice inventory is empty");
}

const sections = [
  "Rootlight Web UI third-party notices",
  "",
  "Generated from package-lock.json and license files shipped by the exact installed packages.",
  "",
];
for (const record of records) {
  sections.push(`${record.name} ${record.version} (${record.license})`, "");
  for (const licenseFile of record.licenseFiles) {
    sections.push(`[${licenseFile.name}]`, "", licenseFile.text, "");
  }
}
await writeFile(output, `${sections.join("\n").trimEnd()}\n`, {
  encoding: "utf8",
  flag: "wx",
});

function normalizeText(value) {
  return value.replaceAll("\r\n", "\n").replaceAll("\r", "\n").trimEnd();
}

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

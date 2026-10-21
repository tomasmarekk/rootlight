/**
 * Observe the real production build's reviewed Lightning CSS native interface.
 * Preload with Node's --require flag when running Vite. Canonical archive hashes
 * remain mandatory; explicit derived selection separately identifies loaded bytes.
 * Any new native export or option requires a fresh advisory reachability review.
 */
const crypto = require("node:crypto");
const fs = require("node:fs");
const Module = require("node:module");
const path = require("node:path");
const { types } = require("node:util");

const project = path.resolve(__dirname, "..");
const frontend = path.join(project, "apps/rootlight-web/frontend");
const closureBytes = fs.readFileSync(path.join(project, "source-inputs/npm/npm-source-closure.json"));
const closure = JSON.parse(closureBytes);
const selection = readSelection();
const loadNative = Module._extensions[".node"];
const loadJavaScript = Module._extensions[".js"];
const publicEntries = new Map();
for (const distribution of closure.packages.filter((entry) => entry.name === "lightningcss")) {
  const member = distribution.members.find((entry) => entry.path === "package/node/index.js");
  for (const installed of distribution.lock_paths) {
    publicEntries.set(filenameKey(path.join(frontend, installed, "node/index.js")), member?.sha256);
  }
}
const observations = [];
const selectedArchiveInputs = new Map();
let calls = 0;
const optionNames = new Set(["code", "cssModules", "customAtRules", "filename", "minify", "targets", "visitor"]);
const browserNames = new Set(["android", "chrome", "edge", "firefox", "ie", "ios_saf", "opera", "safari", "samsung"]);

function filenameKey(filename) {
  return process.platform === "win32" ? filename.toLowerCase() : filename;
}

function requireSelection(condition, detail) {
  if (!condition) throw new Error("Lightning CSS derived selection rejected: " + detail);
}

function exactKeys(value, names) {
  return value && typeof value === "object" && !Array.isArray(value) &&
    JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...names].sort());
}

function readSelection() {
  const marker = process.env.ROOTLIGHT_REBUILT_LIGHTNING_STDIN;
  if (marker === undefined) return null;
  requireSelection(marker === "1", "unsupported explicit selection marker");
  const chunks = [];
  let total = 0;
  for (;;) {
    const chunk = Buffer.alloc(4096);
    const count = fs.readSync(0, chunk, 0, chunk.length, null);
    if (count === 0) break;
    total += count;
    requireSelection(total <= 65536, "capsule exceeds its explicit protocol bound");
    chunks.push(chunk.subarray(0, count));
  }
  const payload = Buffer.concat(chunks);
  const value = JSON.parse(payload.toString("utf8"));
  requireSelection(exactKeys(value, ["schema", "target", "package", "artifact", "runtime", "source",
    "compiler_versions", "authorization_scope"]) && value.schema === "rootlight.lightning-derived-selection/1",
    "capsule schema differs");
  const platforms = {
    "x86_64-pc-windows-msvc": ["win32", "x64", "lightningcss-win32-x64-msvc"],
    "x86_64-unknown-linux-gnu": ["linux", "x64", "lightningcss-linux-x64-gnu"],
    "aarch64-unknown-linux-gnu": ["linux", "arm64", "lightningcss-linux-arm64-gnu"],
    "x86_64-apple-darwin": ["darwin", "x64", "lightningcss-darwin-x64"],
    "aarch64-apple-darwin": ["darwin", "arm64", "lightningcss-darwin-arm64"],
  };
  const target = Object.hasOwn(platforms, value.target) && platforms[value.target];
  requireSelection(target && target[0] === process.platform && target[1] === process.arch,
    "target does not match the actual Node process");
  const runtime = value.runtime;
  requireSelection(exactKeys(runtime, ["version", "platform", "arch", "napi", "modules"]) &&
    runtime.version === process.version && runtime.platform === process.platform && runtime.arch === process.arch &&
    runtime.napi === process.versions.napi && runtime.modules === process.versions.modules,
    "actual Node version or ABI differs");
  const distribution = closure.packages.find((entry) => entry.name === target[2]);
  requireSelection(exactKeys(value.package, ["name", "version", "lock_paths"]) && distribution &&
    value.package.name === distribution.name && value.package.version === distribution.version &&
    JSON.stringify(value.package.lock_paths) === JSON.stringify(distribution.lock_paths),
    "canonical platform package association differs");
  const sourceNames = ["npm_report_sha256", "producer_report_sha256", "cargo_report_sha256",
    "effective_native_report_sha256", "effective_sources_sha256", "five_lockfiles_sha256", "native_build_evidence_sha256"];
  requireSelection(exactKeys(value.source, sourceNames) && sourceNames.every((name) =>
    typeof value.source[name] === "string" && /^[0-9a-f]{64}$/.test(value.source[name])) &&
    value.source.npm_report_sha256 === crypto.createHash("sha256").update(closureBytes).digest("hex"),
    "source metadata or installed npm source frame differs");
  requireSelection(exactKeys(value.compiler_versions, ["cargo", "rustc"]) &&
    Object.values(value.compiler_versions).every((version) => typeof version === "string" && version.length > 0) &&
    value.authorization_scope === "one child selection only; compilation proof belongs to the fresh in-process builder",
    "compiler metadata or authorization scope differs");
  requireSelection(exactKeys(value.artifact, ["path", "bytes", "sha256"]) &&
    typeof value.artifact.path === "string" && path.isAbsolute(value.artifact.path) &&
    Number.isSafeInteger(value.artifact.bytes) && value.artifact.bytes > 0 &&
    typeof value.artifact.sha256 === "string" && /^[0-9a-f]{64}$/.test(value.artifact.sha256),
    "derived artifact identity is invalid");
  // This metadata is an explicit load authorization, never a compilation attestation.
  return { value, sha256: crypto.createHash("sha256").update(payload).digest("hex"), used: false };
}

function verifyDerivedArtifact() {
  const artifact = selection.value.artifact;
  const filename = artifact.path;
  requireSelection(filenameKey(path.resolve(filename)) === filenameKey(filename) &&
    filenameKey(fs.realpathSync(filename)) === filenameKey(filename), "derived path has an alias or linked ancestor");
  const before = fs.lstatSync(filename, { bigint: true });
  requireSelection(before.isFile() && !before.isSymbolicLink() && before.nlink === 1n &&
    before.size === BigInt(artifact.bytes), "derived artifact is not an ordinary unlinked file");
  const descriptor = fs.openSync(filename, "r");
  try {
    const opened = fs.fstatSync(descriptor, { bigint: true });
    const bytes = fs.readFileSync(descriptor);
    const after = fs.fstatSync(descriptor, { bigint: true });
    const pathname = fs.lstatSync(filename, { bigint: true });
    const identity = ["dev", "ino", "mode", "nlink", "size", "mtimeNs", "ctimeNs"];
    requireSelection([opened, after, pathname].every((stat) => identity.every((key) => stat[key] === before[key])) &&
      bytes.length === artifact.bytes && crypto.createHash("sha256").update(bytes).digest("hex") === artifact.sha256,
      "derived artifact changed");
  } finally {
    fs.closeSync(descriptor);
  }
}

function rejectInvocation() {
  throw new Error("Lightning CSS native invocation exceeds the reviewed minification interface");
}

function copyDataRecord(value, allowedNames) {
  if (!value || typeof value !== "object" || types.isProxy(value)) rejectInvocation();
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== null && prototype !== Object.prototype) rejectInvocation();
  const copy = Object.create(null);
  for (const name of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, name);
    if (typeof name !== "string" || !allowedNames.has(name) ||
        !descriptor.enumerable || !Object.hasOwn(descriptor, "value")) rejectInvocation();
    copy[name] = descriptor.value;
  }
  return copy;
}

function reviewedOptions(value) {
  const options = copyDataRecord(value, optionNames);
  if (options.cssModules !== undefined || options.visitor !== undefined ||
      options.customAtRules !== undefined || options.minify !== true ||
      typeof options.filename !== "string" || types.isProxy(options.code) ||
      !Buffer.isBuffer(options.code)) rejectInvocation();
  // Vite supplies disabled own data placeholders. Remove them from the fresh
  // record so neither the upstream wrapper nor native code receives those APIs.
  delete options.visitor;
  delete options.customAtRules;
  if (options.targets !== undefined) {
    options.targets = copyDataRecord(options.targets, browserNames);
    for (const version of Object.values(options.targets)) {
      if (!Number.isInteger(version) || version < 0 || version > 0xffffffff) rejectInvocation();
    }
  }
  // Native property lookup sees inherited properties too. Fresh records keep
  // getters and prototype pollution outside the reviewed options interface.
  return options;
}

Module._extensions[".js"] = function observePublicInterface(module, filename) {
  const key = filenameKey(filename);
  if (!publicEntries.has(key)) return loadJavaScript(module, filename);
  const digest = crypto.createHash("sha256").update(fs.readFileSync(filename)).digest("hex");
  if (digest !== publicEntries.get(key)) {
    throw new Error("Lightning CSS JavaScript interface differs from its authenticated archive member");
  }
  if (selection) selectedArchiveInputs.set(filename, digest);
  loadJavaScript(module, filename);
  // The upstream wrapper reads visitor before calling native code. Validate here
  // as well so accessors cannot run before the native export guard sees options.
  for (const name of ["transform", "transformStyleAttribute", "bundle", "bundleAsync"]) {
    const implementation = module.exports[name];
    const observed = function observePublicCall(...arguments_) {
      if (name !== "transform" || arguments_.length !== 1) rejectInvocation();
      return Reflect.apply(implementation, this, [reviewedOptions(arguments_[0])]);
    };
    module.exports[name] = observed;
    if (typeof implementation !== "function" || module.exports[name] !== observed) {
      throw new Error("Lightning CSS JavaScript observation could not bind an export");
    }
  }
};

Module._extensions[".node"] = function observeNative(module, filename) {
  if (!path.basename(filename).startsWith("lightningcss.")) {
    return loadNative(module, filename);
  }
  const installed = path.relative(frontend, path.dirname(filename)).replaceAll("\\", "/");
  const distribution = closure.packages.find((entry) => entry.lock_paths.includes(installed));
  const member = distribution?.native_binary_files.find((entry) => entry.path === `package/${path.basename(filename)}`);
  const digest = crypto.createHash("sha256").update(fs.readFileSync(filename)).digest("hex");
  if (!member || digest !== member.sha256) {
    throw new Error("Lightning CSS native artifact differs from its authenticated archive member");
  }
  if (selection) selectedArchiveInputs.set(filename, digest);
  const archive = { package: distribution.name, version: distribution.version,
    archive: distribution.archive_path, member: member.path, sha256: digest };
  if (selection) {
    requireSelection(!selection.used && distribution.name === selection.value.package.name &&
      distribution.version === selection.value.package.version && selection.value.package.lock_paths.includes(installed) &&
      filenameKey(path.resolve(filename)) !== filenameKey(selection.value.artifact.path),
      "derived selection does not match this single canonical native load");
    verifyDerivedArtifact();
    loadNative(module, selection.value.artifact.path);
    selection.used = true;
    observations.push({ archive_validation: archive,
      selected_native: { kind: "fresh_in_process_derived", ...selection.value.artifact,
        target: selection.value.target, runtime: selection.value.runtime, source: selection.value.source,
        compiler_versions: selection.value.compiler_versions } });
  } else {
    loadNative(module, filename);
    observations.push(archive);
  }
  for (const name of Object.keys(module.exports)) {
    const native = module.exports[name];
    if (typeof native !== "function") continue;
    const observed = function observeCall(...arguments_) {
      if (name !== "transform" || arguments_.length !== 1) rejectInvocation();
      const options = reviewedOptions(arguments_[0]);
      calls += 1;
      return Reflect.apply(native, this, [options]);
    };
    module.exports[name] = observed;
    if (module.exports[name] !== observed) {
      throw new Error("Lightning CSS native observation could not bind an export");
    }
  }
};

process.on("exit", () => {
  if (observations.length !== 1 || calls < 1) {
    process.stderr.write("Lightning CSS boundary check did not observe the expected production minifier\n");
    process.exitCode = 1;
  }
  if (selection) {
    try {
      requireSelection(selection.used && observations.length === 1 && calls >= 1, "derived load was not observed");
      verifyDerivedArtifact();
      for (const [filename, digest] of selectedArchiveInputs) {
        requireSelection(crypto.createHash("sha256").update(fs.readFileSync(filename)).digest("hex") === digest,
          "authenticated archive input changed after the derived load");
      }
    } catch (error) {
      process.stderr.write(String(error) + "\n");
      process.exitCode = 1;
    }
  }
  process.stdout.write(JSON.stringify({ schema: selection ? "rootlight.lightning-frontend-boundary/2" : "rootlight.lightning-frontend-boundary/1",
    transform_calls: calls, native_artifacts: observations,
    ...(selection ? { derived_selection_observed: selection.used,
      selection_capsule_sha256: selection.sha256 } : {}),
    source_to_binary_rebuild_verified: false, embedded_dependency_versions_attested: false }) + "\n");
});

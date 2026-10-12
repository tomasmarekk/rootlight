/**
 * Observe the real production build's reviewed Lightning CSS native interface.
 * Preload with Node's --require flag when running Vite. Archive member hashes
 * identify the selected artifact; they do not attest its embedded Rust versions.
 * Any new native export or option requires a fresh advisory reachability review.
 */
const crypto = require("node:crypto");
const fs = require("node:fs");
const Module = require("node:module");
const path = require("node:path");
const { types } = require("node:util");

const project = path.resolve(__dirname, "..");
const frontend = path.join(project, "apps/rootlight-web/frontend");
const closure = JSON.parse(fs.readFileSync(path.join(project, "source-inputs/npm/npm-source-closure.json")));
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
let calls = 0;
const optionNames = new Set(["code", "cssModules", "filename", "minify", "targets"]);
const browserNames = new Set(["android", "chrome", "edge", "firefox", "ie", "ios_saf", "opera", "safari", "samsung"]);

function filenameKey(filename) {
  return process.platform === "win32" ? filename.toLowerCase() : filename;
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
  if (options.cssModules !== undefined || options.minify !== true ||
      typeof options.filename !== "string" || types.isProxy(options.code) ||
      !Buffer.isBuffer(options.code)) rejectInvocation();
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
  loadNative(module, filename);
  observations.push({ package: distribution.name, version: distribution.version,
    archive: distribution.archive_path, member: member.path, sha256: digest });
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
  process.stdout.write(JSON.stringify({ schema: "rootlight.lightning-frontend-boundary/1",
    transform_calls: calls, native_artifacts: observations,
    source_to_binary_rebuild_verified: false, embedded_dependency_versions_attested: false }) + "\n");
});

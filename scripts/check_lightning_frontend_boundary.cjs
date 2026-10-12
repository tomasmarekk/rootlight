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

const project = path.resolve(__dirname, "..");
const frontend = path.join(project, "apps/rootlight-web/frontend");
const closure = JSON.parse(fs.readFileSync(path.join(project, "source-inputs/npm/npm-source-closure.json")));
const loadNative = Module._extensions[".node"];
const observations = [];
let calls = 0;

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
      const options = arguments_[0];
      const allowed = new Set(["code", "cssModules", "filename", "minify", "targets"]);
      if (name !== "transform" || arguments_.length !== 1 || !options ||
          Object.keys(options).some((key) => !allowed.has(key)) ||
          options.cssModules !== undefined || options.minify !== true ||
          typeof options.filename !== "string" || !Buffer.isBuffer(options.code)) {
        throw new Error("Lightning CSS native invocation exceeds the reviewed minification interface");
      }
      calls += 1;
      return Reflect.apply(native, this, arguments_);
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

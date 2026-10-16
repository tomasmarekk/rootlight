/**
 * Verify explicit derived selection against a fresh installed package copy.
 * The caller supplies a capsule retained from the in-process builder; replay here
 * tests load authorization and rejection only, never proves compilation again.
 */
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const Module = require("node:module");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

const project = path.resolve(__dirname, "..");
const capsulePath = process.env.ROOTLIGHT_TEST_DERIVED_CAPSULE;
if (!capsulePath) throw new Error("Provide an explicit freshly produced capsule for the installed-loader controls");
const capsule = JSON.parse(fs.readFileSync(capsulePath));
const closure = JSON.parse(fs.readFileSync(path.join(project, "source-inputs/npm/npm-source-closure.json")));
const wrapper = closure.packages.find((entry) => entry.name === "lightningcss");
const platformPackage = closure.packages.find((entry) => entry.name === capsule.package.name);
assert.ok(wrapper && platformPackage);
const wrapperPath = wrapper.lock_paths.find((entry) => platformPackage.lock_paths.some((native) =>
  path.posix.dirname(native) === path.posix.dirname(entry)));
assert.ok(wrapperPath, "canonical native and public packages need the same resolution placement");
const nativeMember = platformPackage.native_binary_files.find((entry) =>
  path.posix.basename(entry.path) === path.basename(capsule.artifact.path));
assert.ok(nativeMember);

function canonicalRuntimePlacements() {
  const frontend = path.join(project, "apps/rootlight-web/frontend");
  const queue = [wrapperPath, ...platformPackage.lock_paths.filter((placement) =>
    fs.existsSync(path.join(frontend, placement)))];
  const selected = new Set();
  while (queue.length) {
    const placement = queue.shift();
    if (selected.has(placement)) continue;
    const manifest = path.join(frontend, placement, "package.json");
    const declared = JSON.parse(fs.readFileSync(manifest));
    const authenticated = closure.packages.find((entry) => entry.name === declared.name &&
      entry.version === declared.version && entry.lock_paths.includes(placement));
    assert.ok(authenticated, "runtime package must match its canonical archive placement");
    selected.add(placement);
    const resolve = Module.createRequire(manifest);
    for (const dependency of Object.keys(declared.dependencies || {})) {
      const dependencyManifest = resolve.resolve(dependency + "/package.json");
      const relative = path.relative(frontend, path.dirname(dependencyManifest)).replaceAll("\\", "/");
      assert.ok(relative.startsWith("node_modules/") && !relative.split("/").includes(".."),
        "runtime dependency resolved outside this installed frontend");
      queue.push(relative);
    }
  }
  return [...selected];
}
const runtimePlacements = canonicalRuntimePlacements();

function fixture(context) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rootlight-derived-loader-"));
  context.after(() => fs.rmSync(root, { recursive: true }));
  const frontend = path.join(root, "apps/rootlight-web/frontend");
  const guard = path.join(root, "scripts/check_lightning_frontend_boundary.cjs");
  fs.mkdirSync(path.dirname(guard), { recursive: true });
  fs.copyFileSync(path.join(__dirname, "check_lightning_frontend_boundary.cjs"), guard);
  const report = path.join(root, "source-inputs/npm/npm-source-closure.json");
  fs.mkdirSync(path.dirname(report), { recursive: true });
  fs.copyFileSync(path.join(project, "source-inputs/npm/npm-source-closure.json"), report);
  for (const placement of runtimePlacements) {
    const original = path.join(project, "apps/rootlight-web/frontend", placement);
    fs.cpSync(original, path.join(frontend, placement), { recursive: true });
  }
  const artifact = path.join(root, path.basename(capsule.artifact.path));
  fs.copyFileSync(capsule.artifact.path, artifact);
  const selection = structuredClone(capsule);
  selection.artifact.path = artifact;
  return { root, frontend, guard, artifact, selection, wrapper: path.join(frontend, wrapperPath, "node/index.js"),
    registry: path.join(frontend, platformPackage.lock_paths[0], path.posix.basename(nativeMember.path)) };
}

function run(fix, selection = fix.selection, directNative = false) {
  const load = `const css=require(${JSON.stringify(directNative ? fix.registry : fix.wrapper)});`;
  const checks = `
    const result=css.transform({code:Buffer.from('.foo { color: transparent; }'),filename:'derived.css',minify:true,targets:{android:95<<16}});
    if(result.code.toString()!=='.foo{color:#0000}') throw new Error('unexpected real CSS output');
    let rejected=false;
    try { css.transform({code:Buffer.from('.foo'),filename:'invalid.css',minify:true,targets:{}}); }
    catch(error) { rejected=error instanceof SyntaxError && error.message==='Unexpected end of input' &&
      error.fileName==='invalid.css' && error.source==='.foo' && error.loc.line===1 && error.loc.column===5; }
    if(!rejected) throw new Error('native parse error path did not reject');
  `;
  const environment = { ...process.env, ROOTLIGHT_REBUILT_LIGHTNING_STDIN: "1" };
  delete environment.NODE_OPTIONS;
  delete environment.NODE_PATH;
  return spawnSync(process.execPath, ["--require", fix.guard, "--eval", load + checks],
    { cwd: fix.root, env: environment, input: JSON.stringify(selection) + "\n", encoding: "utf8" });
}

test("the actual installed public loader selects derived bytes and performs real transform/error paths", (context) => {
  const fix = fixture(context);
  const before = crypto.createHash("sha256").update(fs.readFileSync(fix.registry)).digest("hex");
  const result = run(fix);
  assert.equal(result.status, 0, result.stderr);
  const observed = JSON.parse(result.stdout.trim());
  assert.equal(observed.schema, "rootlight.lightning-frontend-boundary/2");
  assert.equal(observed.transform_calls, 2);
  assert.equal(observed.derived_selection_observed, true);
  assert.equal(observed.source_to_binary_rebuild_verified, false);
  assert.equal(observed.native_artifacts[0].archive_validation.sha256, before);
  assert.equal(observed.native_artifacts[0].selected_native.sha256, capsule.artifact.sha256);
  assert.equal(crypto.createHash("sha256").update(fs.readFileSync(fix.registry)).digest("hex"), before);
});

test("changed derived bytes reject before native loading", (context) => {
  const fix = fixture(context);
  const bytes = fs.readFileSync(fix.artifact);
  bytes[0] ^= 1;
  fs.writeFileSync(fix.artifact, bytes);
  const result = run(fix, fix.selection, true);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /derived artifact changed/);
});

test("the original registry native member remains mandatory before derived loading", (context) => {
  const fix = fixture(context);
  const bytes = fs.readFileSync(fix.registry);
  bytes[0] ^= 1;
  fs.writeFileSync(fix.registry, bytes);
  const result = run(fix, fix.selection, true);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /native artifact differs from its authenticated archive member/);
});

test("the original public wrapper hash remains mandatory before derived loading", (context) => {
  const fix = fixture(context);
  fs.appendFileSync(fix.wrapper, "\n// changed interface bytes\n");
  const result = run(fix);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /JavaScript interface differs from its authenticated archive member/);
});

test("wrong runtime ABI, source report or target rejects the explicit selection", (context) => {
  const fix = fixture(context);
  for (const edit of [
    (value) => { value.runtime.modules = "0"; },
    (value) => { value.source.npm_report_sha256 = "0".repeat(64); },
    (value) => { value.target = "unsupported-target"; },
  ]) {
    const value = structuredClone(fix.selection);
    edit(value);
    const result = run(fix, value);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /derived selection rejected/);
  }
});

test("a persisted receipt without an artifact is not a loadable selection", (context) => {
  const fix = fixture(context);
  fs.unlinkSync(fix.artifact);
  const result = run(fix, fix.selection, true);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /ENOENT/);
});

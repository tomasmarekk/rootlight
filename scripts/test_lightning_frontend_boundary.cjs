/** Verify the boundary against the installed platform artifact before release. */
const assert = require("node:assert/strict");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

const project = path.resolve(__dirname, "..");
const guard = path.join(__dirname, "check_lightning_frontend_boundary.cjs");
const load = "const css=require('./apps/rootlight-web/frontend/node_modules/vite/node_modules/lightningcss');";

function run(invocation) {
  return spawnSync(process.execPath, ["--require", guard, "--eval", load + invocation],
    { cwd: project, encoding: "utf8", timeout: 30000 });
}

test("the pinned native transform accepts the reviewed CSS minification interface", () => {
  const result = run("css.transform({code:Buffer.from('a { color: red }'),filename:'fixture.css',minify:true,targets:{}});");
  assert.equal(result.status, 0, result.stderr);
  const receipt = JSON.parse(result.stdout.trim());
  assert.equal(receipt.transform_calls, 1);
  assert.equal(receipt.native_artifacts.length, 1);
  assert.equal(receipt.embedded_dependency_versions_attested, false);
});

test("bundling is rejected before the native implementation can read a file", () => {
  const result = run("css.bundle({filename:'does-not-exist.css',minify:true});");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /exceeds the reviewed minification interface/);
  assert.equal(JSON.parse(result.stdout.trim()).transform_calls, 0);
});

test("source maps and JavaScript visitors require a fresh native security review", () => {
  for (const option of ["sourceMap:true", "inputSourceMap:'{}'", "visitor:{}", "cssModules:true"]) {
    const result = run(`css.transform({code:Buffer.from('a{}'),filename:'fixture.css',minify:true,${option}});`);
    assert.equal(result.status, 1, option);
    assert.match(result.stderr, /exceeds the reviewed minification interface/);
    assert.equal(JSON.parse(result.stdout.trim()).transform_calls, 0);
  }
});

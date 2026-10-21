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

function assertRejectedWithoutSideEffects(setup, call = "css.transform(options)") {
  const result = run(`let sideEffects=0;${setup}
    try { ${call}; }
    finally { process.stderr.write('side_effects='+sideEffects+'\\n'); }`);
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /exceeds the reviewed minification interface/);
  assert.match(result.stderr, /side_effects=0\n/);
  assert.equal(JSON.parse(result.stdout.trim()).transform_calls, 0);
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

test("inherited visitors never enter the retained native transform", () => {
  assertRejectedWithoutSideEffects(`
    const options=Object.assign(Object.create({visitor:{Declaration(){sideEffects++;}}}),
      {code:Buffer.from('a{color:red}'),filename:'fixture.css',minify:true,targets:{}});`);
});

test("ES module imports retain the guard before upstream visitor access", () => {
  const result = run(`import('./apps/rootlight-web/frontend/node_modules/vite/node_modules/lightningcss/node/index.mjs')
    .then((esm) => {
      let visits=0;
      const options={code:Buffer.from('a{}'),filename:'fixture.css',minify:true,targets:{}};
      Object.defineProperty(options,'visitor',{enumerable:true,get(){visits++;return {};}});
      try { esm.transform(options); }
      finally { process.stderr.write('visitor_reads='+visits+'\\n'); }
    });`);
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /exceeds the reviewed minification interface/);
  assert.match(result.stderr, /visitor_reads=0\n/);
  assert.equal(JSON.parse(result.stdout.trim()).transform_calls, 0);
});

test("direct native exports independently reject accessor options", () => {
  assertRejectedWithoutSideEffects(`
    const native=Object.values(require.cache).find((entry) => /lightningcss\\..*\\.node$/.test(entry.filename)).exports;
    const options={code:Buffer.from('a{}'),minify:true,targets:{}};
    Object.defineProperty(options,'filename',{enumerable:true,get(){sideEffects++;return 'fixture.css';}});`,
    "native.transform(options)");
});

test("option accessors, hidden properties, symbols, and proxies reject without callbacks", () => {
  const base = "const options={code:Buffer.from('a{color:red}'),filename:'fixture.css',minify:true,targets:{}};";
  for (const property of [
    "Object.defineProperty(options,'filename',{enumerable:true,get(){sideEffects++;return 'fixture.css';}});",
    "Object.defineProperty(options,'visitor',{enumerable:true,get(){sideEffects++;return {};}});",
    "Object.defineProperty(options,'visitor',{value:{Declaration(){sideEffects++;}}});",
    "options.visitor=()=>{sideEffects++;return {};};",
    "options[Symbol('visitor')]={};",
  ]) assertRejectedWithoutSideEffects(base + property);
  assertRejectedWithoutSideEffects(`const options=new Proxy(
    {code:Buffer.from('a{}'),filename:'fixture.css',minify:true,targets:{}},
    {getPrototypeOf(target){sideEffects++;return Reflect.getPrototypeOf(target);},
     ownKeys(target){sideEffects++;return Reflect.ownKeys(target);}});`);
});

test("browser targets reject inherited, accessor, hidden, proxy, and invalid values", () => {
  for (const targets of [
    "Object.create({chrome:65536})",
    "Object.defineProperty({},'chrome',{enumerable:true,get(){sideEffects++;return 65536;}})",
    "Object.defineProperty({},'chrome',{value:65536})",
    "new Proxy({chrome:65536},{ownKeys(target){sideEffects++;return Reflect.ownKeys(target);}})",
    "{[Symbol('chrome')]:65536}",
    "{unknown:65536}",
    "{chrome:-1}",
    "{chrome:4294967296}",
    "{chrome:1.5}",
    "{chrome:'65536'}",
  ]) assertRejectedWithoutSideEffects(`const options={code:Buffer.from('a{}'),filename:'fixture.css',minify:true,targets:${targets}};`);
});

test("plain and null-prototype records preserve minification without inherited callbacks", () => {
  const result = run(`let visits=0;Object.prototype.visitor={Declaration(){visits++;}};
    try {
      const targets=Object.assign(Object.create(null),{chrome:65536});
      const options=Object.assign(Object.create(null),{code:Buffer.from('a{color:red}'),filename:'fixture.css',minify:true,targets});
      const result=css.transform(options);
      if (!Buffer.isBuffer(result.code) || visits!==0) throw new Error('unreviewed callback or invalid result');
      css.transform({code:Buffer.from('a{}'),filename:'fixture.css',minify:true,targets:{}});
      if (visits!==0) throw new Error('inherited visitor executed');
    } finally { delete Object.prototype.visitor; }`);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout.trim()).transform_calls, 2);
});

test("disabled own data placeholders preserve the native minification interface", () => {
  const result = run(`
    const native=Object.values(require.cache).find((entry) => /lightningcss\\..*\\.node$/.test(entry.filename)).exports;
    for (const prototype of [Object.prototype,null]) {
      const options=Object.assign(Object.create(prototype),{code:Buffer.from('a{color:red}'),
        filename:'fixture.css',minify:true,targets:{},visitor:undefined,customAtRules:undefined});
      const result=css.transform(options);
      if (!Buffer.isBuffer(result.code) || !Object.hasOwn(options,'visitor') ||
          !Object.hasOwn(options,'customAtRules')) throw new Error('invalid result or mutated caller options');
    }
    native.transform({code:Buffer.from('a{}'),filename:'fixture.css',minify:true,
      visitor:undefined,customAtRules:undefined});`);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout.trim()).transform_calls, 3);
});

test("nonundefined visitor and custom rule values cannot reach native code", () => {
  const base = "const options={code:Buffer.from('a{}'),filename:'fixture.css',minify:true,targets:{}};";
  for (const name of ["visitor", "customAtRules"]) {
    for (const value of ["null", "false", "{}", "()=>{sideEffects++;}",
      "new Proxy({},{get(){sideEffects++;return undefined;},ownKeys(){sideEffects++;return [];}})"]) {
      assertRejectedWithoutSideEffects(base + `options.${name}=${value};`);
    }
  }
});

test("disabled placeholders still reject accessors hidden properties proxies and symbols", () => {
  const base = "const options={code:Buffer.from('a{}'),filename:'fixture.css',minify:true,targets:{}};";
  for (const name of ["visitor", "customAtRules"]) {
    assertRejectedWithoutSideEffects(base +
      `Object.defineProperty(options,'${name}',{enumerable:true,get(){sideEffects++;return undefined;}});`);
    assertRejectedWithoutSideEffects(base + `Object.defineProperty(options,'${name}',{value:undefined});`);
    assertRejectedWithoutSideEffects(base + `options[Symbol('${name}')]=undefined;`);
    assertRejectedWithoutSideEffects(base +
      `const native=Object.values(require.cache).find((entry) => /lightningcss\\..*\\.node$/.test(entry.filename)).exports;
       Object.defineProperty(options,'${name}',{enumerable:true,get(){sideEffects++;return undefined;}});`,
      "native.transform(options)");
  }
  assertRejectedWithoutSideEffects(`const options=new Proxy(
    {code:Buffer.from('a{}'),filename:'fixture.css',minify:true,visitor:undefined,customAtRules:undefined},
    {getPrototypeOf(target){sideEffects++;return Reflect.getPrototypeOf(target);},
     ownKeys(target){sideEffects++;return Reflect.ownKeys(target);}});`);
});

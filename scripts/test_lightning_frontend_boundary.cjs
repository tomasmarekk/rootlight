/** Verify the boundary against the installed platform artifact before release. */
const assert = require("node:assert/strict");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const { runInNewContext } = require("node:vm");

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

function instrumentDerivedArtifact(filename, contents, hooks = {}) {
  const source = fs.readFileSync(guard, "utf8");
  const definitions = ["filenameKey", "requireSelection", "verifyDerivedArtifact"].map((name) => {
    const matches = [...source.matchAll(new RegExp(`^function ${name}\\([^]*?^\\}`, "gm"))];
    assert.equal(matches.length, 1, `${name} must retain one standalone definition`);
    return matches[0][0];
  }).join("\n");
  const calls = { opened: [], closed: [], allocations: [], reads: 0, bytes: 0 };
  const verify = runInNewContext(`${definitions}; verifyDerivedArtifact`, {
    crypto, path, process,
    selection: { value: { artifact: { path: filename, bytes: contents.length,
      sha256: crypto.createHash("sha256").update(contents).digest("hex") } } },
    Buffer: { alloc(size) {
      calls.allocations.push(size);
      assert.ok(size <= 64 * 1024, "artifact reads must use bounded scratch space");
      return Buffer.alloc(size);
    } },
    fs: { ...fs, ...hooks,
      openSync(file, flags) {
        const descriptor = (hooks.openSync ?? fs.openSync)(file, flags);
        calls.opened.push(descriptor);
        return descriptor;
      },
      closeSync(descriptor) {
        calls.closed.push(descriptor);
        fs.closeSync(descriptor);
      },
      readFileSync() { assert.fail("artifact admission must precede every bounded read"); },
      readSync(...args) {
        calls.reads += 1;
        const count = (hooks.readSync ?? fs.readSync)(...args);
        calls.bytes += count;
        return count;
      },
    },
  });
  return { verify, calls };
}

test("derived artifact verification authenticates exact bytes with bounded scratch and closes handles", () => {
  const directory = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rootlight-lightning-artifact-")));
  try {
    const filename = path.join(directory, "artifact.node");
    for (const size of [1, 64 * 1024 + 17]) {
      const contents = Buffer.alloc(size, 42);
      fs.writeFileSync(filename, contents);
      const { verify, calls } = instrumentDerivedArtifact(filename, contents);
      verify();
      assert.equal(calls.bytes, size);
      assert.ok(calls.reads >= 2);
      assert.deepEqual(calls.closed, calls.opened);
      assert.equal(calls.opened.length, 1);
    }
  } finally {
    fs.rmSync(directory, { recursive: true });
  }
});

test("derived artifact rejects oversized linked and replaced paths before allocating or reading", () => {
  const directory = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rootlight-lightning-artifact-")));
  try {
    const filename = path.join(directory, "artifact.node");
    const contents = Buffer.from("reviewed artifact");
    const rejected = (hooks = {}) => {
      const { verify, calls } = instrumentDerivedArtifact(filename, contents, hooks);
      assert.throws(verify, /derived artifact|metadata unavailable|ELOOP|EISDIR/);
      assert.deepEqual(calls.allocations, []);
      assert.equal(calls.reads, 0);
      assert.deepEqual(calls.closed, calls.opened);
    };
    fs.writeFileSync(filename, Buffer.concat([contents, Buffer.from("extra")]));
    rejected();
    fs.unlinkSync(filename);
    fs.mkdirSync(filename);
    rejected();
    fs.rmdirSync(filename);
    fs.writeFileSync(filename, contents);
    const hardlink = path.join(directory, "hardlink");
    fs.linkSync(filename, hardlink);
    rejected();
    fs.unlinkSync(hardlink);
    for (const replacement of [contents, Buffer.concat([contents, Buffer.from("extra")])]) {
      const held = path.join(directory, `held-${replacement.length}`);
      rejected({ openSync(file, flags) {
        const descriptor = fs.openSync(file, flags);
        fs.renameSync(file, held);
        fs.writeFileSync(file, replacement);
        return descriptor;
      } });
      fs.unlinkSync(filename);
      fs.renameSync(held, filename);
    }
    rejected({ lstatSync() { throw new Error("metadata unavailable"); } });
    if (process.platform !== "win32") {
      fs.unlinkSync(filename);
      const target = path.join(directory, "target");
      fs.writeFileSync(target, contents);
      fs.symlinkSync(target, filename);
      rejected();
      rejected({ constants: { ...fs.constants, O_NOFOLLOW: 0 } });
    }
  } finally {
    fs.rmSync(directory, { recursive: true });
  }
});

test("derived artifact rejects growth truncation hash changes and late path replacement", () => {
  const directory = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rootlight-lightning-artifact-")));
  try {
    const filename = path.join(directory, "artifact.node");
    const contents = Buffer.from("reviewed artifact");
    const mutations = [() => fs.appendFileSync(filename, "extra"),
      () => fs.truncateSync(filename, 0),
      () => fs.writeFileSync(filename, Buffer.alloc(contents.length, 88)),
      () => { fs.renameSync(filename, path.join(directory, "held")); fs.writeFileSync(filename, contents); }];
    for (const mutate of mutations) {
      fs.writeFileSync(filename, contents);
      let changed = false;
      const { verify, calls } = instrumentDerivedArtifact(filename, contents, { readSync(...args) {
        if (!changed) { changed = true; mutate(); }
        return fs.readSync(...args);
      } });
      assert.throws(verify, /derived artifact/);
      assert.ok(calls.bytes <= contents.length + 1);
      assert.deepEqual(calls.closed, calls.opened);
      assert.equal(calls.opened.length, 1);
    }
  } finally {
    fs.rmSync(directory, { recursive: true });
  }
});

test("derived artifact opening cannot wait for a substituted FIFO writer", () => {
  const directory = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rootlight-lightning-artifact-")));
  try {
    const filename = path.join(directory, "artifact.node");
    const fifo = path.join(directory, "fifo");
    const contents = Buffer.from("reviewed artifact");
    fs.writeFileSync(filename, contents);
    const { verify, calls } = instrumentDerivedArtifact(filename, contents, { fstatSync(...args) {
      return { ...fs.fstatSync(...args), isFile() { return false; } };
    } });
    assert.throws(verify, /derived artifact/);
    assert.equal(calls.reads, 0);
    assert.deepEqual(calls.allocations, []);
    assert.deepEqual(calls.closed, calls.opened);
    assert.equal(calls.opened.length, 1);
    if (process.platform === "win32") return;
    const created = spawnSync("mkfifo", [fifo], { encoding: "utf8" });
    assert.equal(created.status, 0, created.stderr);
    const result = spawnSync(process.execPath, ["--eval", `
      const assert=require('node:assert/strict'), fs=require('node:fs'), crypto=require('node:crypto');
      const path=require('node:path'), {runInNewContext}=require('node:vm');
      const guard=${JSON.stringify(guard)};
      ${instrumentDerivedArtifact.toString()}
      const filename=${JSON.stringify(filename)}, fifo=${JSON.stringify(fifo)};
      const contents=Buffer.from('reviewed artifact');
      const {verify,calls}=instrumentDerivedArtifact(filename,contents,{openSync(file,flags){
        fs.renameSync(fifo,file); return fs.openSync(file,flags);
      }});
      assert.throws(verify,/derived artifact/); assert.equal(calls.reads,0);
      assert.deepEqual(calls.allocations,[]); assert.deepEqual(calls.closed,calls.opened);
      assert.equal(calls.opened.length,1);
    `], { encoding: "utf8", timeout: 5000 });
    assert.equal(result.error, undefined, result.error?.message);
    assert.equal(result.status, 0, result.stderr);
  } finally {
    fs.rmSync(directory, { recursive: true });
  }
});

// Exercises release-origin authentication with tiny authored tar distributions.
// Fixtures model separately reviewed member evidence; no npm, Python, network,
// installation or preferred-source verification is performed by these tests.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, symlink, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { gzipSync } from "node:zlib";
import { verifyLocalDependencyOrigins } from "./verify-local-dependency-origins.mjs";

const LOCK = "apps/rootlight-web/frontend/package-lock.json";
const UPSTREAM = "source-inputs/npm/upstream-package-lock.json";
const MANIFEST = "source-inputs/npm/local-distributions.json";
const REPORT = "source-inputs/npm/npm-source-closure.json";
const LOCAL = "source-inputs/npm/local-distributions/";
const ORIGINALS = "source-inputs/npm/archives/";
const FALSE = [
  "installation_verified",
  "build_and_packaging_acceptance_verified",
  "os_denied_acceptance_verified",
  "preferred_source_closure_complete",
  "frontend_source_complete",
  "provenance_signatures_verified",
];
const PATCH_FIELDS = [
  "member",
  "offset",
  "removed_length",
  "removed_sha256",
  "replacement_length",
  "replacement_sha256",
];

function digest(bytes, algorithm = "sha256") {
  return createHash(algorithm).update(bytes).digest("hex");
}
function sri(bytes, algorithm = "sha512") {
  return algorithm + "-" + createHash(algorithm).update(bytes).digest("base64");
}
function json(value) {
  return Buffer.from(JSON.stringify(value, null, 2) + "\n");
}
function canonical(value) {
  if (Array.isArray(value)) return "[" + value.map(canonical).join(",") + "]";
  if (value !== null && typeof value === "object")
    return (
      "{" +
      Object.keys(value)
        .sort()
        .map((key) => JSON.stringify(key) + ":" + canonical(value[key]))
        .join(",") +
      "}"
    );
  return JSON.stringify(value);
}
function archive(name, fixtureText, { unsorted = false, paxSize = false } = {}) {
  const contents = [
    ["authored/LICENSE", Buffer.from("Authored fixture license\n")],
    ["authored/package.json", json({ name, version: "1.2.3", license: "MIT" })],
    ["authored/tests/value.test.js", Buffer.from(fixtureText)],
  ];
  if (unsorted) contents.unshift(contents.pop());
  const records = contents.flatMap(([path, bytes]) => {
    if (!paxSize || path !== "authored/tests/value.test.js") return [[path, bytes, "0"]];
    const body = "size=" + bytes.length + "\n";
    let length = Buffer.byteLength(body) + 3;
    while (length !== Buffer.byteLength(length + " " + body))
      length = Buffer.byteLength(length + " " + body);
    return [
      ["authored/PaxHeaders/value.test.js", Buffer.from(length + " " + body), "x"],
      [path, bytes, "0"],
    ];
  });
  const blocks = [];
  const members = [];
  for (const [path, bytes, type] of records) {
    const header = Buffer.alloc(512);
    header.write(path, 0, 100, "utf8");
    for (const [offset, length, value] of [
      [100, 8, 0o644],
      [108, 8, 0],
      [116, 8, 0],
      [124, 12, bytes.length],
      [136, 12, 100],
    ]) {
      header.write(value.toString(8).padStart(length - 1, "0") + "\0", offset, length, "ascii");
    }
    header.fill(32, 148, 156);
    header.write(type, 156);
    header.write("ustar\0", 257);
    header.write("00", 263);
    const checksum = header.reduce((sum, value) => sum + value, 0);
    header.write(checksum.toString(8).padStart(6, "0") + "\0 ", 148, 8, "ascii");
    blocks.push(header, bytes, Buffer.alloc((512 - (bytes.length % 512)) % 512));
    if (type === "0")
      members.push({
        path,
        kind: "file",
        bytes: bytes.length,
        archive_mode: 0o644,
        sha256: digest(bytes),
      });
  }
  blocks.push(Buffer.alloc(1024));
  const emptyHash = digest(Buffer.alloc(0));
  const metadata = members.map((member) => ({
    path: member.path,
    type_hex: "30",
    mode: 0o644,
    bytes: member.bytes,
    uid: 0,
    gid: 0,
    mtime: 100,
    uname_sha256: emptyHash,
    gname_sha256: emptyHash,
    linkname_sha256: emptyHash,
    pax_headers_sha256: digest(
      Buffer.from(
        paxSize && member.path === "authored/tests/value.test.js"
          ? canonical({ size: String(member.bytes) })
          : "{}",
      ),
    ),
    pax_non_size_headers_sha256: digest(Buffer.from("{}")),
    pax_size: paxSize && member.path === "authored/tests/value.test.js" ? member.bytes : null,
  }));
  members.sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));
  return { payload: gzipSync(Buffer.concat(blocks), { mtime: 0 }), members, contents, metadata };
}
async function fixture(
  t,
  { variants = true, unsorted = false, paxSize = false, replacement = "after!" } = {},
) {
  const root = await mkdtemp(join(tmpdir(), "release-origin-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const old = archive("toolkit", "before fixture", { unsorted, paxSize });
  const changed = archive("toolkit", replacement + " fixture", { unsorted, paxSize });
  const platform = archive("@scope/platform", "before fixture", { unsorted, paxSize });
  const platformChanged = archive("@scope/platform", replacement + " fixture", {
    unsorted,
    paxSize,
  });
  const first = {
    version: "1.2.3",
    resolved: "https://registry.npmjs.org/toolkit/-/toolkit-1.2.3.tgz",
    integrity: sri(old.payload),
    dev: true,
    license: "MIT",
  };
  const second = {
    version: "1.2.3",
    resolved: "https://registry.npmjs.org/@scope/platform/-/platform-1.2.3.tgz",
    integrity: sri(platform.payload),
    optional: true,
    os: ["linux"],
    cpu: ["x64"],
  };
  const upstream = {
    name: "authored-ui",
    version: "1.0.0",
    lockfileVersion: 3,
    requires: true,
    packages: {
      "": { name: "authored-ui", version: "1.0.0", dependencies: { toolkit: "1.2.3" } },
      "node_modules/toolkit": first,
      "node_modules/container/node_modules/toolkit": { ...first, optional: true },
      "node_modules/@scope/platform": second,
    },
  };
  const upstreamBytes = json(upstream);
  const pins = [
    {
      name: "@scope/platform",
      version: "1.2.3",
      resolved: second.resolved,
      integrity: second.integrity,
      archive_filename: digest(Buffer.from(second.resolved)) + ".tgz",
      lock_paths: ["node_modules/@scope/platform"],
      lock_records: [
        { path: "node_modules/@scope/platform", optional: true, os: ["linux"], cpu: ["x64"] },
      ],
    },
    {
      name: "toolkit",
      version: "1.2.3",
      resolved: first.resolved,
      integrity: first.integrity,
      archive_filename: digest(Buffer.from(first.resolved)) + ".tgz",
      lock_paths: ["node_modules/container/node_modules/toolkit", "node_modules/toolkit"],
      lock_records: [
        {
          path: "node_modules/container/node_modules/toolkit",
          dev: true,
          optional: true,
          license: "MIT",
        },
        { path: "node_modules/toolkit", dev: true, license: "MIT" },
      ],
    },
  ];
  const files = new Map([[UPSTREAM, upstreamBytes]]);
  const manifest = {
    schema: "rootlight.npm-local-distributions/1",
    upstream_lock_sha256: digest(upstreamBytes),
    variants: [],
  };
  const packages = [];
  const projection = structuredClone(upstream);
  let provenance;
  for (const pin of pins) {
    const variant = variants && (pin.name === "toolkit" || variants === "all");
    const previousArchive = pin.name === "toolkit" ? old : platform;
    const input = variant ? (pin.name === "toolkit" ? changed : platformChanged) : previousArchive;
    const path = variant ? LOCAL + pin.archive_filename : ORIGINALS + pin.archive_filename;
    const record = {
      ...pin,
      resolved: "file:../../../" + path,
      integrity: variant ? sri(input.payload) : pin.integrity,
      archive_filename: path.split("/").at(-1),
      archive_path: path,
      archive: {
        bytes: input.payload.length,
        sha256: digest(input.payload),
        sha512: digest(input.payload, "sha512"),
        sri_verified_algorithm: "sha512",
      },
      archive_root: "authored",
      member_count: input.members.length,
      members: input.members,
      members_sha256: digest(Buffer.from(canonical(input.members))),
      package_json_sha256: digest(
        input.contents.find(([path]) => path === "authored/package.json")[1],
      ),
      license_metadata: { license: "MIT" },
      provenance_metadata: {},
      license_files: [input.members[0]],
      native_binary_files: [],
      upstream_pin: pin,
      distribution_kind: variant ? "locally_modified_distribution" : "original_registry_archive",
      upstream_original_sri_recomputed: !variant,
    };
    files.set(path, input.payload);
    if (variant) {
      const previous = previousArchive.members[2];
      const current = input.members[2];
      const patch = {
        member: current.path,
        member_sha256_before: previous.sha256,
        member_sha256_after: current.sha256,
        member_computed_sri_sha512_before: sri(
          previousArchive.contents.find(([path]) => path === previous.path)[1],
        ),
        member_computed_sri_sha512_after: sri(
          input.contents.find(([path]) => path === current.path)[1],
        ),
        member_bytes_before: previous.bytes,
        member_bytes_after: current.bytes,
        offset: 0,
        removed_length: 6,
        removed_sha256: digest(Buffer.from("before")),
        replacement_ascii: replacement,
        replacement_length: Buffer.byteLength(replacement),
        replacement_sha256: digest(Buffer.from(replacement)),
        matching_window_count: 1,
      };
      provenance = {
        schema: "rootlight.npm-local-fixture-patch/1",
        lockfile_sha256: digest(upstreamBytes),
        original_pin: pin,
        archive_before: {
          bytes: previousArchive.payload.length,
          sha256: digest(previousArchive.payload),
          sha512: digest(previousArchive.payload, "sha512"),
          sri_verified_algorithm: "sha512",
          computed_sri_sha512: sri(previousArchive.payload),
          locked_integrity: pin.integrity,
        },
        archive_after: {
          ...record.archive,
          computed_sri_sha512: sri(input.payload),
          filename: "patched.tgz",
        },
        patch,
        archive_root: record.archive_root,
        package_json_sha256: record.package_json_sha256,
        member_count: record.member_count,
        member_inventory_before: previousArchive.members,
        member_inventory_after: record.members,
        member_metadata_before: previousArchive.metadata,
        member_metadata_after: input.metadata,
        unchanged_other_member_count: 2,
        license_files_preserved: record.license_files,
        native_binary_files_preserved: [],
        verification_scope:
          "one exact caller-approved fixture window; all other logical tar members unchanged",
        source_scope:
          "locally modified npm distribution only; no installation, preferred-source, or authorship claim",
        ...Object.fromEntries(
          [
            "locally_modified_distribution",
            "original_archive_unchanged",
            "original_locked_sri_verified",
            "patched_archive_self_integrity_verified",
            "synthetic_replacement_confirmed_by_caller",
            "other_members_bytes_and_metadata_preserved",
          ].map((key) => [key, true]),
        ),
        ...Object.fromEntries(
          [
            "patched_archive_is_original_upstream_archive",
            "canonical_lock_updated",
            "locked_install_verified",
            "preferred_source_closure_complete",
            "frontend_source_complete",
            "os_blocked_network_verified",
            "provenance_signatures_verified",
          ].map((key) => [key, false]),
        ),
      };
      const provenancePath = LOCAL + pin.archive_filename + ".provenance.json";
      const provenanceBytes = json(provenance);
      files.set(provenancePath, provenanceBytes);
      manifest.variants.push({
        name: pin.name,
        version: pin.version,
        archive_path: path,
        archive_sha256: record.archive.sha256,
        provenance_path: provenancePath,
        provenance_sha256: digest(provenanceBytes),
      });
      Object.assign(record, {
        fixture_patch: Object.fromEntries(PATCH_FIELDS.map((key) => [key, patch[key]])),
        provenance_path: provenancePath,
        provenance_sha256: digest(provenanceBytes),
        original_sri_verified_during_preparation: true,
      });
    }
    for (const path of pin.lock_paths) {
      projection.packages[path].resolved = record.resolved;
      if (variant) projection.packages[path].integrity = record.integrity;
    }
    packages.push(record);
  }
  files.set(LOCK, json(projection));
  files.set(MANIFEST, json(manifest));
  const report = {
    schema: "rootlight.npm-local-archive-bundle/1",
    lockfile_sha256: digest(files.get(LOCK)),
    upstream_lockfile_sha256: digest(upstreamBytes),
    local_distributions_sha256: digest(files.get(MANIFEST)),
    lock_record_count: 3,
    archive_count: 2,
    original_archive_count: 2 - manifest.variants.length,
    local_distribution_count: manifest.variants.length,
    archives_verified: true,
    packages,
    archive_paths: packages.map((record) => record.archive_path).sort(),
    provenance_paths: manifest.variants.map((variant) => variant.provenance_path).sort(),
    input_files: [...files]
      .map(([path, bytes]) => ({ path, sha256: digest(bytes) }))
      .sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0)),
    upstream_original_sri_recomputed: !variants,
    ...Object.fromEntries(FALSE.map((key) => [key, false])),
    verification_scope:
      "exact local lock projection and every retained original or declared modified distribution",
    historical_preparation_scope:
      "removed upstream archive SRI, before-member hashes and unique removed window are hash-bound preparation evidence, not current byte recomputation",
    source_scope:
      "retained npm distributions only; preferred native, WASI and bundled transitive sources remain unproven",
    licensing_scope:
      "supplied metadata and preserved files; not legal completeness or authorship evidence",
  };
  files.set(REPORT, json(report));
  await mkdir(resolve(root, ORIGINALS), { recursive: true });
  await mkdir(resolve(root, LOCAL), { recursive: true });
  for (const [path, bytes] of files) {
    await mkdir(resolve(root, path, ".."), { recursive: true });
    await writeFile(resolve(root, path), bytes);
  }
  return {
    root,
    files,
    upstream,
    canonical: projection,
    manifest,
    report,
    provenance,
    async write(path, value) {
      await writeFile(resolve(root, path), json(value));
    },
    async verify() {
      return verifyLocalDependencyOrigins(root, await readFile(resolve(root, LOCK)));
    },
  };
}

async function rebindProvenance(f) {
  const variant = f.manifest.variants.find(
    (item) =>
      item.name === f.provenance.original_pin.name &&
      item.version === f.provenance.original_pin.version,
  );
  const record = f.report.packages.find(
    (item) => item.name === variant.name && item.version === variant.version,
  );
  const bytes = json(f.provenance);
  await writeFile(resolve(f.root, variant.provenance_path), bytes);
  variant.provenance_sha256 = digest(bytes);
  record.provenance_sha256 = digest(bytes);
  const manifestBytes = json(f.manifest);
  await writeFile(resolve(f.root, MANIFEST), manifestBytes);
  f.report.local_distributions_sha256 = digest(manifestBytes);
  for (const input of f.report.input_files) {
    if (input.path === variant.provenance_path) input.sha256 = digest(bytes);
    if (input.path === MANIFEST) input.sha256 = digest(manifestBytes);
  }
  await f.write(REPORT, f.report);
}

test("original TAR order permits fixture length and PAX-size deltas without inventory reordering", async (t) => {
  for (const paxSize of [false, true]) {
    for (const replacement of ["new", "approved replacement"]) {
      const f = await fixture(t, { unsorted: true, paxSize, replacement });
      assert.deepEqual(
        f.provenance.member_metadata_before.map((item) => item.path),
        ["authored/tests/value.test.js", "authored/LICENSE", "authored/package.json"],
      );
      assert.deepEqual(
        f.provenance.member_inventory_before.map((item) => item.path),
        ["authored/LICENSE", "authored/package.json", "authored/tests/value.test.js"],
      );
      const before = f.provenance.member_metadata_before[0];
      const after = f.provenance.member_metadata_after[0];
      assert.equal(after.bytes - before.bytes, replacement.length - 6);
      assert.equal(after.pax_size, paxSize ? after.bytes : null);
      assert.equal(after.pax_non_size_headers_sha256, before.pax_non_size_headers_sha256);
      if (paxSize) assert.notEqual(after.pax_headers_sha256, before.pax_headers_sha256);
      const result = await f.verify();
      assert.deepEqual(result, {
        lockfileSha256: digest(f.files.get(LOCK)),
        reportSha256: digest(f.files.get(REPORT)),
        archiveCount: 2,
        lockRecordCount: 3,
      });
      for (const [path, bytes] of f.files)
        assert.deepEqual(await readFile(resolve(f.root, path)), bytes);
    }
  }
});

test("metadata own paths, order and non-size fields stay exact after all provenance pins are rebound", async (t) => {
  for (const mode of [
    "unrelated",
    "order",
    "duplicate",
    "fixture-mode",
    "fixture-time",
    "pax-size",
    "pax-other",
  ]) {
    const f = await fixture(t, { unsorted: true, paxSize: true });
    assert.equal((await f.verify()).archiveCount, 2);
    const target = f.provenance.member_metadata_after.find(
      (item) => item.path === f.provenance.patch.member,
    );
    if (mode === "unrelated")
      f.provenance.member_metadata_after.find(
        (item) => item.path === "authored/package.json",
      ).uid += 1;
    if (mode === "order") f.provenance.member_metadata_after.reverse();
    if (mode === "duplicate") f.provenance.member_metadata_after[2] = { ...target };
    if (mode === "fixture-mode") target.mode = 0o755;
    if (mode === "fixture-time") target.mtime += 1;
    if (mode === "pax-size") target.pax_size += 1;
    if (mode === "pax-other") target.pax_non_size_headers_sha256 = "0".repeat(64);
    await rebindProvenance(f);
    await assert.rejects(f.verify(), /origin validation failed/u);
  }
});

test("multiple declared variants independently preserve unsorted metadata and length deltas", async (t) => {
  const f = await fixture(t, {
    variants: "all",
    unsorted: true,
    paxSize: true,
    replacement: "new",
  });
  assert.equal(f.manifest.variants.length, 2);
  assert.equal(f.report.original_archive_count, 0);
  assert.equal((await f.verify()).archiveCount, 2);
});

test("all original and declared local origins preserve every nested/optional placement", async (t) => {
  for (const variants of [false, true, "all"]) {
    const f = await fixture(t, { variants });
    const result = await f.verify();
    assert.deepEqual(result, {
      lockfileSha256: digest(f.files.get(LOCK)),
      reportSha256: digest(f.files.get(REPORT)),
      archiveCount: 2,
      lockRecordCount: 3,
    });
    for (const [path, bytes] of f.files)
      assert.deepEqual(await readFile(resolve(f.root, path)), bytes);
  }
});
test("remote, traversal, escaped and foreign local origins are rejected", async (t) => {
  for (const origin of [
    "https://registry.npmjs.org/toolkit/-/toolkit-1.2.3.tgz",
    "file:../../../source-inputs/npm/archives/../outside.tgz",
    "file:../../../source-inputs/npm/archives/%2e%2e/outside.tgz",
    "file:///absolute.tgz",
    "file:../../../source-inputs/npm/local-distributions/foreign.tgz",
  ]) {
    const f = await fixture(t);
    f.canonical.packages["node_modules/toolkit"].resolved = origin;
    await f.write(LOCK, f.canonical);
    await assert.rejects(f.verify(), /origin validation failed/u);
  }
});
test("version, root, placement, flags and nested record metadata cannot be rebound", async (t) => {
  for (const mutate of [
    (f) => {
      f.canonical.packages["node_modules/toolkit"].version = "1.2.4";
    },
    (f) => {
      f.canonical.packages[""].dependencies.toolkit = "latest";
    },
    (f) => {
      delete f.canonical.packages["node_modules/container/node_modules/toolkit"];
    },
    (f) => {
      f.canonical.packages["node_modules/@scope/platform"].optional = false;
    },
  ]) {
    const f = await fixture(t);
    mutate(f);
    await f.write(LOCK, f.canonical);
    f.report.lockfile_sha256 = digest(json(f.canonical));
    f.report.input_files.find((record) => record.path === LOCK).sha256 = f.report.lockfile_sha256;
    await f.write(REPORT, f.report);
    await assert.rejects(f.verify(), /origin validation failed/u);
  }
});
test("strongest SRI, actual bytes, missing optional archives and undeclared extras fail closed", async (t) => {
  for (const mode of ["sri", "bytes", "missing", "extra", "weak"]) {
    const f = await fixture(t, { variants: false });
    const record = f.report.packages[0];
    if (mode === "bytes")
      await writeFile(resolve(f.root, record.archive_path), Buffer.from("changed"));
    if (mode === "missing") await unlink(resolve(f.root, record.archive_path));
    if (mode === "extra")
      await writeFile(resolve(f.root, ORIGINALS, "extra.tgz"), Buffer.from("extra"));
    if (mode === "sri" || mode === "weak") {
      const payload = f.files.get(record.archive_path);
      const value =
        mode === "weak"
          ? sri(payload, "sha1")
          : sri(payload, "sha256") + " " + sri(Buffer.from("wrong"));
      const lockPath = record.lock_paths[0];
      f.upstream.packages[lockPath].integrity = value;
      f.canonical.packages[lockPath].integrity = value;
      await f.write(UPSTREAM, f.upstream);
      await f.write(LOCK, f.canonical);
      f.manifest.upstream_lock_sha256 = digest(json(f.upstream));
      await f.write(MANIFEST, f.manifest);
      f.report.upstream_lockfile_sha256 = digest(json(f.upstream));
      f.report.lockfile_sha256 = digest(json(f.canonical));
      f.report.local_distributions_sha256 = digest(json(f.manifest));
      await f.write(REPORT, f.report);
    }
    await assert.rejects(f.verify(), /origin validation failed/u);
  }
});
test("report omissions, extra inputs, false acceptance and provenance tampering are rejected", async (t) => {
  for (const mode of [
    "package",
    "placement",
    "input",
    "extra",
    "acceptance",
    "provenance",
    "rebound",
  ]) {
    const f = await fixture(t);
    if (mode === "package") f.report.packages.pop();
    if (mode === "placement") f.report.packages[1].lock_paths.pop();
    if (mode === "input") f.report.input_files.pop();
    if (mode === "extra") f.report.input_files.push({ path: "other.json", sha256: "0".repeat(64) });
    if (mode === "acceptance") f.report.preferred_source_closure_complete = true;
    if (mode === "provenance")
      await f.write(f.manifest.variants[0].provenance_path, { invalid: true });
    if (mode === "rebound") {
      f.provenance.original_pin.version = "2.0.0";
      const bytes = json(f.provenance);
      await writeFile(resolve(f.root, f.manifest.variants[0].provenance_path), bytes);
      f.manifest.variants[0].provenance_sha256 = digest(bytes);
      await f.write(MANIFEST, f.manifest);
      f.report.local_distributions_sha256 = digest(json(f.manifest));
      f.report.packages[1].provenance_sha256 = digest(bytes);
      for (const record of f.report.input_files) {
        if (record.path === MANIFEST) record.sha256 = digest(json(f.manifest));
        if (record.path === f.manifest.variants[0].provenance_path) record.sha256 = digest(bytes);
      }
    }
    await f.write(REPORT, f.report);
    await assert.rejects(f.verify(), /origin validation failed/u);
  }
});
test("duplicate manifest variants and duplicate raw JSON keys are rejected", async (t) => {
  const f = await fixture(t);
  f.manifest.variants.push({ ...f.manifest.variants[0] });
  await f.write(MANIFEST, f.manifest);
  await assert.rejects(f.verify(), /origin validation failed/u);
  const g = await fixture(t);
  const contents = (await readFile(resolve(g.root, MANIFEST), "utf8")).replace(
    '"schema":',
    '"schema":"foreign","schema":',
  );
  await writeFile(resolve(g.root, MANIFEST), contents);
  await assert.rejects(g.verify(), /origin validation failed/u);
});

test("omitting any individual canonical placement or the whole inventory is rejected", async (t) => {
  const first = await fixture(t);
  const paths = Object.keys(first.canonical.packages).filter((path) => path !== "");
  for (const path of [...paths, null]) {
    const f = await fixture(t);
    if (path === null) f.canonical.packages = { "": f.canonical.packages[""] };
    else assert.equal(Reflect.deleteProperty(f.canonical.packages, path), true);
    await f.write(LOCK, f.canonical);
    f.report.lockfile_sha256 = digest(json(f.canonical));
    f.report.input_files.find((record) => record.path === LOCK).sha256 = f.report.lockfile_sha256;
    await f.write(REPORT, f.report);
    await assert.rejects(f.verify(), /origin validation failed/u);
  }
});
test("caller snapshot and typed lock metadata cannot be silently replaced", async (t) => {
  const f = await fixture(t);
  await assert.rejects(
    verifyLocalDependencyOrigins(f.root, Buffer.from("{}")),
    /origin validation failed/u,
  );
  const g = await fixture(t, { variants: false });
  g.upstream.packages[""].authoredNumber = 1;
  g.canonical.packages[""].authoredNumber = 1;
  await g.write(UPSTREAM, g.upstream);
  const contents = json(g.canonical)
    .toString("utf8")
    .replace('"authoredNumber": 1', '"authoredNumber": 1.0');
  await writeFile(resolve(g.root, LOCK), contents);
  g.manifest.upstream_lock_sha256 = digest(json(g.upstream));
  await g.write(MANIFEST, g.manifest);
  g.report.upstream_lockfile_sha256 = digest(json(g.upstream));
  g.report.lockfile_sha256 = digest(Buffer.from(contents));
  g.report.local_distributions_sha256 = digest(json(g.manifest));
  for (const record of g.report.input_files) {
    if (record.path === UPSTREAM) record.sha256 = g.report.upstream_lockfile_sha256;
    if (record.path === LOCK) record.sha256 = g.report.lockfile_sha256;
    if (record.path === MANIFEST) record.sha256 = g.report.local_distributions_sha256;
  }
  await g.write(REPORT, g.report);
  await assert.rejects(g.verify(), /origin validation failed/u);
});
test("manifest path aliases, traversal, and foreign URL pins are rejected before use", async (t) => {
  for (const mode of ["traversal", "percent", "absolute", "foreign", "unmapped", "control"]) {
    const f = await fixture(t);
    if (mode === "traversal") f.manifest.variants[0].archive_path = LOCAL + "../outside.tgz";
    if (mode === "percent") f.manifest.variants[0].provenance_path = LOCAL + "%2e%2e/outside.json";
    if (mode === "absolute") f.manifest.variants[0].archive_path = "/outside.tgz";
    if (mode === "control") f.manifest.variants[0].archive_path = LOCAL + "bad\n.tgz";
    if (mode === "unmapped") f.manifest.variants[0].name = "unlocked-name";
    if (mode === "foreign") {
      f.upstream.packages["node_modules/@scope/platform"].resolved =
        "https://foreign.invalid/platform.tgz";
      await f.write(UPSTREAM, f.upstream);
      f.manifest.upstream_lock_sha256 = digest(json(f.upstream));
    }
    await f.write(MANIFEST, f.manifest);
    await assert.rejects(f.verify(), /origin validation failed/u);
  }
});

test("symlinked inputs and linked project ancestors are not admitted", async (t) => {
  const f = await fixture(t);
  const path = f.report.packages[0].archive_path;
  const external = join(f.root, "external.tgz");
  await writeFile(external, f.files.get(path));
  await unlink(resolve(f.root, path));
  try {
    await symlink(external, resolve(f.root, path));
  } catch (error) {
    if (["EPERM", "EACCES", "ENOSYS"].includes(error.code)) {
      t.skip("host does not permit authored symlinks");
      return;
    }
    throw error;
  }
  await assert.rejects(f.verify(), /origin validation failed/u);
  const alias = join(f.root, "linked-root");
  await symlink(f.root, alias, process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(
    verifyLocalDependencyOrigins(alias, f.files.get(LOCK)),
    /origin validation failed/u,
  );
});

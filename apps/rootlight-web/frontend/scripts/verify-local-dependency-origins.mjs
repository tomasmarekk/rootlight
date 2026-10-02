// Authenticates release dependency origins against the reviewed local lock projection.
// Archive-member and license preservation remain separately verified report metadata;
// this check establishes current bytes/SRI, not preferred-source or offline acceptance.

import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { lstat, open, readdir } from "node:fs/promises";
import { isAbsolute, parse, resolve, sep } from "node:path";
import { TextDecoder } from "node:util";
import { isDeepStrictEqual } from "node:util";

const LOCK = "apps/rootlight-web/frontend/package-lock.json";
const UPSTREAM = "source-inputs/npm/upstream-package-lock.json";
const MANIFEST = "source-inputs/npm/local-distributions.json";
const REPORT = "source-inputs/npm/npm-source-closure.json";
const ORIGINALS = "source-inputs/npm/archives";
const LOCAL = "source-inputs/npm/local-distributions";
const PACKAGE_LIMIT = 2048;
const JSON_LIMIT = 16 * 1024 * 1024;
const ARCHIVE_LIMIT = 128 * 1024 * 1024;
const HASH = /^[0-9a-f]{64}$/u;
const SHA512 = /^[0-9a-f]{128}$/u;
const NAME = /^[a-z0-9][a-z0-9._-]*$/u;
const VERSION =
  /^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u;
const STRENGTH = { sha1: 1, sha256: 2, sha384: 3, sha512: 4 };
const FALSE = [
  "installation_verified",
  "build_and_packaging_acceptance_verified",
  "os_denied_acceptance_verified",
  "preferred_source_closure_complete",
  "frontend_source_complete",
  "provenance_signatures_verified",
];
const PROVENANCE_TRUE = [
  "locally_modified_distribution",
  "original_archive_unchanged",
  "original_locked_sri_verified",
  "patched_archive_self_integrity_verified",
  "synthetic_replacement_confirmed_by_caller",
  "other_members_bytes_and_metadata_preserved",
];
const PROVENANCE_FALSE = [
  "patched_archive_is_original_upstream_archive",
  "canonical_lock_updated",
  "locked_install_verified",
  "preferred_source_closure_complete",
  "frontend_source_complete",
  "os_blocked_network_verified",
  "provenance_signatures_verified",
];
const REPORT_KEYS = [
  "schema",
  "lockfile_sha256",
  "upstream_lockfile_sha256",
  "local_distributions_sha256",
  "lock_record_count",
  "archive_count",
  "original_archive_count",
  "local_distribution_count",
  "archives_verified",
  "packages",
  "archive_paths",
  "provenance_paths",
  "input_files",
  "upstream_original_sri_recomputed",
  ...FALSE,
  "verification_scope",
  "historical_preparation_scope",
  "source_scope",
  "licensing_scope",
];
const PACKAGE_KEYS = [
  "archive",
  "archive_filename",
  "archive_root",
  "integrity",
  "name",
  "version",
  "resolved",
  "lock_paths",
  "lock_records",
  "member_count",
  "members",
  "members_sha256",
  "package_json_sha256",
  "license_metadata",
  "provenance_metadata",
  "license_files",
  "native_binary_files",
  "archive_path",
  "upstream_pin",
  "distribution_kind",
  "upstream_original_sri_recomputed",
];
const PATCH_KEYS = [
  "member",
  "member_sha256_before",
  "member_sha256_after",
  "member_computed_sri_sha512_before",
  "member_computed_sri_sha512_after",
  "member_bytes_before",
  "member_bytes_after",
  "offset",
  "removed_length",
  "removed_sha256",
  "replacement_ascii",
  "replacement_length",
  "replacement_sha256",
  "matching_window_count",
];
const PATCH_REPORT_KEYS = [
  "member",
  "offset",
  "removed_length",
  "removed_sha256",
  "replacement_length",
  "replacement_sha256",
];
const PROVENANCE_KEYS = [
  "schema",
  "lockfile_sha256",
  "original_pin",
  "archive_before",
  "archive_after",
  "patch",
  "archive_root",
  "package_json_sha256",
  "member_count",
  "member_inventory_before",
  "member_inventory_after",
  "member_metadata_before",
  "member_metadata_after",
  "unchanged_other_member_count",
  "license_files_preserved",
  "native_binary_files_preserved",
  "verification_scope",
  "source_scope",
  ...PROVENANCE_TRUE,
  ...PROVENANCE_FALSE,
];

/**
 * Validate fixed repository-local origin inputs without writes or subprocesses.
 * projectRoot must be an absolute, unlinked directory. canonicalLockBytes binds
 * the caller's already-read lock to this check. Returns the bound input/report
 * SHA-256 and inventory counts;
 * throws a bounded, source-free error on malformed, missing, changed or foreign input.
 * Reviewed versioned inputs are the trust anchor; reports cannot self-authenticate.
 */
export async function verifyLocalDependencyOrigins(projectRoot, canonicalLockBytes) {
  try {
    check(
      typeof projectRoot === "string" && isAbsolute(projectRoot),
      "absolute project root required",
    );
    check(
      !projectRoot.split(/[\\/]/u).some((part) => part === "." || part === ".."),
      "root traversal",
    );
    const root = resolve(projectRoot);
    await checkedPath(root, true);
    const inputs = new Inputs(root);
    const upstreamInput = await inputs.json(UPSTREAM);
    const canonicalInput = await inputs.json(LOCK);
    const manifestInput = await inputs.json(MANIFEST);
    const reportInput = await inputs.json(REPORT);
    check(
      Buffer.isBuffer(canonicalLockBytes) && canonicalLockBytes.equals(canonicalInput.contents),
      "caller lock changed",
    );
    const upstream = upstreamInput.document;
    const canonical = canonicalInput.document;
    const manifest = manifestInput.document;
    const report = reportInput.document;
    const identities = lockedArchives(upstream);
    exactKeys(manifest, ["schema", "upstream_lock_sha256", "variants"]);
    check(
      manifest.schema === "rootlight.npm-local-distributions/1" &&
        manifest.upstream_lock_sha256 === upstreamInput.sha256 &&
        Array.isArray(manifest.variants) &&
        manifest.variants.length <= identities.length,
      "manifest identity",
    );
    exactKeys(report, REPORT_KEYS);
    check(
      report.schema === "rootlight.npm-local-archive-bundle/1" &&
        report.archives_verified === true &&
        FALSE.every((field) => report[field] === false),
      "report claims",
    );
    check(
      report.lockfile_sha256 === canonicalInput.sha256 &&
        report.upstream_lockfile_sha256 === upstreamInput.sha256 &&
        report.local_distributions_sha256 === manifestInput.sha256,
      "report input hashes",
    );
    check(
      report.verification_scope ===
        "exact local lock projection and every retained original or declared modified distribution" &&
        report.historical_preparation_scope ===
          "removed upstream archive SRI, before-member hashes and unique removed window are hash-bound preparation evidence, not current byte recomputation" &&
        report.source_scope ===
          "retained npm distributions only; preferred native, WASI and bundled transitive sources remain unproven" &&
        report.licensing_scope ===
          "supplied metadata and preserved files; not legal completeness or authorship evidence",
      "report scope",
    );

    const identityKeys = new Set(identities.map(identityKey));
    const variants = new Map();
    const localPaths = new Set();
    const aliases = new Map();
    for (const variant of manifest.variants) {
      exactKeys(variant, [
        "name",
        "version",
        "archive_path",
        "archive_sha256",
        "provenance_path",
        "provenance_sha256",
      ]);
      const key = identityKey(variant);
      check(
        identityKeys.has(key) &&
          !variants.has(key) &&
          HASH.test(variant.archive_sha256) &&
          HASH.test(variant.provenance_sha256),
        "variant pin",
      );
      for (const [field, suffix] of [
        ["archive_path", ".tgz"],
        ["provenance_path", ".json"],
      ]) {
        const path = variant[field];
        safePath(path);
        check(
          path.startsWith(LOCAL + "/") &&
            path.endsWith(suffix) &&
            !/[%#]/u.test(path) &&
            !localPaths.has(path),
          "variant path",
        );
        addAliases(path, aliases);
        localPaths.add(path);
      }
      variants.set(key, variant);
    }
    check(
      Array.isArray(report.packages) && report.packages.length === identities.length,
      "report package closure",
    );
    const projection = structuredClone(upstream);
    const originals = new Set();
    const archivePaths = [];
    const provenancePaths = [];
    let placements = 0;
    for (let index = 0; index < identities.length; index += 1) {
      const pin = identities[index];
      const variant = variants.get(identityKey(pin));
      const path = variant?.archive_path ?? ORIGINALS + "/" + pin.archive_filename;
      if (!variant) originals.add(pin.archive_filename);
      archivePaths.push(path);
      const archive = await inputs.file(path, ARCHIVE_LIMIT);
      const integrity = variant ? sri512(archive.sha512) : pin.integrity;
      const algorithm = verifySri(integrity, archive.hashes);
      const record = report.packages[index];
      exactKeys(record, [
        ...PACKAGE_KEYS,
        ...(variant
          ? [
              "fixture_patch",
              "provenance_path",
              "provenance_sha256",
              "original_sri_verified_during_preparation",
            ]
          : []),
      ]);
      check(
        isDeepStrictEqual(record.upstream_pin, pin) &&
          record.name === pin.name &&
          record.version === pin.version &&
          isDeepStrictEqual(record.lock_paths, pin.lock_paths) &&
          isDeepStrictEqual(record.lock_records, pin.lock_records) &&
          record.archive_filename === path.split("/").at(-1) &&
          record.archive_path === path &&
          record.resolved === "file:../../../" + path &&
          record.integrity === integrity &&
          record.distribution_kind ===
            (variant ? "locally_modified_distribution" : "original_registry_archive") &&
          record.upstream_original_sri_recomputed === !variant,
        "report package pin",
      );
      check(
        isDeepStrictEqual(record.archive, {
          bytes: archive.bytes,
          sha256: archive.sha256,
          sha512: archive.sha512,
          sri_verified_algorithm: algorithm,
        }),
        "report archive hashes",
      );
      memberInventory(record.members);
      check(
        Array.isArray(record.members) &&
          integer(record.member_count, true) &&
          record.members.length === record.member_count &&
          record.members_sha256 === hash("sha256", Buffer.from(canonicalJson(record.members))) &&
          HASH.test(record.package_json_sha256) &&
          typeof record.archive_root === "string" &&
          Array.isArray(record.license_files) &&
          Array.isArray(record.native_binary_files),
        "report inventory binding",
      );
      if (variant) {
        check(archive.sha256 === variant.archive_sha256, "variant archive changed");
        const provenanceInput = await inputs.json(variant.provenance_path);
        check(
          provenanceInput.sha256 === variant.provenance_sha256 &&
            record.provenance_path === variant.provenance_path &&
            record.provenance_sha256 === variant.provenance_sha256 &&
            record.original_sri_verified_during_preparation === true,
          "variant provenance hash",
        );
        verifyProvenance(provenanceInput.document, pin, record, upstreamInput.sha256);
        provenancePaths.push(variant.provenance_path);
      }
      for (const path of pin.lock_paths) {
        projection.packages[path].resolved = record.resolved;
        if (variant) projection.packages[path].integrity = integrity;
        placements += 1;
      }
    }
    check(
      isDeepStrictEqual(canonical, projection) &&
        isDeepStrictEqual(canonicalInput.numberKinds, upstreamInput.numberKinds),
      "canonical projection differs",
    );
    check(
      report.lock_record_count === placements &&
        report.archive_count === identities.length &&
        report.original_archive_count === originals.size &&
        report.local_distribution_count === variants.size &&
        report.upstream_original_sri_recomputed === (variants.size === 0) &&
        isDeepStrictEqual(report.archive_paths, archivePaths.sort()) &&
        isDeepStrictEqual(report.provenance_paths, provenancePaths.sort()),
      "report closure counts",
    );
    await inputs.directory(ORIGINALS, originals);
    await inputs.directory(
      LOCAL,
      new Set([...localPaths].map((path) => path.slice(LOCAL.length + 1))),
    );
    const expectedInputs = [...inputs.files]
      .filter(([path]) => path !== REPORT)
      .map(([path, file]) => ({ path, sha256: file.sha256 }))
      .sort((left, right) => compare(left.path, right.path));
    check(isDeepStrictEqual(report.input_files, expectedInputs), "report input inventory");
    await inputs.unchanged();
    return {
      lockfileSha256: canonicalInput.sha256,
      reportSha256: reportInput.sha256,
      archiveCount: identities.length,
      lockRecordCount: placements,
    };
  } catch {
    throw new Error("local npm release origin validation failed");
  }
}

function lockedArchives(lock) {
  check(
    object(lock) &&
      lock.lockfileVersion === 3 &&
      object(lock.packages) &&
      object(lock.packages[""]) &&
      lock.name === lock.packages[""].name &&
      lock.version === lock.packages[""].version,
    "upstream lock",
  );
  packageName(lock.name);
  check(VERSION.test(lock.version), "root version");
  const paths = Object.keys(lock.packages)
    .filter((path) => path !== "")
    .sort();
  check(paths.length > 0 && paths.length <= PACKAGE_LIMIT, "package cap");
  const identities = new Map();
  const aliases = new Map();
  for (const path of paths) {
    safePath(path);
    addAliases(path, aliases);
    const parts = path.split("/");
    let name;
    for (let position = 0; position < parts.length;) {
      check(parts[position++] === "node_modules" && position < parts.length, "lock placement");
      name = parts[position++];
      if (name.startsWith("@")) {
        check(position < parts.length, "scoped placement");
        name += "/" + parts[position++];
      }
      packageName(name);
    }
    const record = lock.packages[path];
    check(
      object(record) &&
        !Object.hasOwn(record, "link") &&
        !Object.hasOwn(record, "inBundle") &&
        VERSION.test(record.version) &&
        (!Object.hasOwn(record, "name") || record.name === name),
      "lock identity",
    );
    const url =
      "https://registry.npmjs.org/" +
      name +
      "/-/" +
      name.split("/").at(-1) +
      "-" +
      record.version +
      ".tgz";
    check(record.resolved === url, "approved registry pin");
    const integrity = parseSri(record.integrity).tokens.join(" ");
    const key = name + "\n" + record.version;
    let identity = identities.get(key);
    if (identity)
      check(identity.resolved === url && identity.integrity === integrity, "conflicting pin");
    else {
      identity = {
        name,
        version: record.version,
        resolved: url,
        integrity,
        archive_filename: hash("sha256", Buffer.from(url)) + ".tgz",
        lock_paths: [],
        lock_records: [],
      };
      identities.set(key, identity);
    }
    const metadata = { path };
    for (const field of ["dev", "optional", "devOptional", "hasInstallScript"]) {
      if (Object.hasOwn(record, field)) {
        check(typeof record[field] === "boolean", "lock boolean");
        metadata[field] = record[field];
      }
    }
    for (const field of ["license", "os", "cpu", "libc"]) {
      if (Object.hasOwn(record, field)) metadata[field] = record[field];
    }
    identity.lock_paths.push(path);
    identity.lock_records.push(metadata);
  }
  return [...identities.values()].sort(
    (left, right) => compare(left.name, right.name) || compare(left.version, right.version),
  );
}

function verifyProvenance(document, pin, record, upstreamHash) {
  exactKeys(document, PROVENANCE_KEYS);
  check(
    document.schema === "rootlight.npm-local-fixture-patch/1" &&
      document.lockfile_sha256 === upstreamHash &&
      isDeepStrictEqual(document.original_pin, pin) &&
      PROVENANCE_TRUE.every((field) => document[field] === true) &&
      PROVENANCE_FALSE.every((field) => document[field] === false),
    "provenance pin/claims",
  );
  check(
    document.verification_scope ===
      "one exact caller-approved fixture window; all other logical tar members unchanged" &&
      document.source_scope ===
        "locally modified npm distribution only; no installation, preferred-source, or authorship claim",
    "provenance scope",
  );
  exactKeys(document.archive_before, [
    "bytes",
    "sha256",
    "sha512",
    "sri_verified_algorithm",
    "computed_sri_sha512",
    "locked_integrity",
  ]);
  const before = document.archive_before;
  check(
    integer(before.bytes, true) &&
      HASH.test(before.sha256) &&
      SHA512.test(before.sha512) &&
      before.computed_sri_sha512 === sri512(before.sha512) &&
      before.locked_integrity === pin.integrity &&
      before.sri_verified_algorithm === parseSri(pin.integrity).strongest &&
      before.sha256 !== record.archive.sha256,
    "historical archive evidence",
  );
  // Before bytes are deliberately absent; only declared historical hashes are bound.
  const beforeHashes = { sha256: before.sha256, sha512: before.sha512 };
  const selected = parseSri(pin.integrity);
  if (Object.hasOwn(beforeHashes, selected.strongest)) verifySri(pin.integrity, beforeHashes);
  check(
    isDeepStrictEqual(document.archive_after, {
      ...record.archive,
      computed_sri_sha512: sri512(record.archive.sha512),
      filename: "patched.tgz",
    }),
    "current provenance archive",
  );
  for (const [field, recordField] of [
    ["archive_root", "archive_root"],
    ["package_json_sha256", "package_json_sha256"],
    ["member_count", "member_count"],
    ["member_inventory_after", "members"],
    ["license_files_preserved", "license_files"],
    ["native_binary_files_preserved", "native_binary_files"],
  ]) {
    check(
      isDeepStrictEqual(document[field], record[recordField]),
      "provenance protected inventory",
    );
  }
  check(
    document.unchanged_other_member_count === record.member_count - 1 &&
      Array.isArray(document.member_inventory_before) &&
      document.member_inventory_before.length === record.member_count &&
      Array.isArray(document.member_metadata_before) &&
      Array.isArray(document.member_metadata_after) &&
      document.member_metadata_before.length === record.member_count &&
      document.member_metadata_after.length === record.member_count,
    "historical inventory",
  );
  const patch = document.patch;
  exactKeys(patch, PATCH_KEYS);
  check(
    isDeepStrictEqual(
      record.fixture_patch,
      Object.fromEntries(PATCH_REPORT_KEYS.map((field) => [field, patch[field]])),
    ),
    "patch report binding",
  );
  for (const field of [
    "member_sha256_before",
    "member_sha256_after",
    "removed_sha256",
    "replacement_sha256",
  ]) {
    check(HASH.test(patch[field]), "patch hash");
  }
  safePath(patch.member);
  check(
    integer(patch.offset) &&
      integer(patch.member_bytes_before) &&
      integer(patch.member_bytes_after) &&
      integer(patch.removed_length, true) &&
      integer(patch.replacement_length, true) &&
      patch.matching_window_count === 1 &&
      typeof patch.replacement_ascii === "string" &&
      /^[\x20-\x7e]+$/u.test(patch.replacement_ascii),
    "patch coordinates",
  );
  check(
    Buffer.byteLength(patch.replacement_ascii) === patch.replacement_length &&
      hash("sha256", Buffer.from(patch.replacement_ascii)) === patch.replacement_sha256 &&
      patch.removed_sha256 !== patch.replacement_sha256 &&
      patch.offset + patch.removed_length <= patch.member_bytes_before &&
      patch.member_bytes_before - patch.removed_length + patch.replacement_length ===
        patch.member_bytes_after,
    "patch delta",
  );
  for (const field of ["member_computed_sri_sha512_before", "member_computed_sri_sha512_after"]) {
    const hashes = parseSri(patch[field]);
    check(hashes.tokens.length === 1 && hashes.strongest === "sha512", "member SRI evidence");
  }
  memberInventory(document.member_inventory_before);
  check(
    isDeepStrictEqual(
      document.member_inventory_before.map((member) => member.path),
      record.members.map((member) => member.path),
    ),
    "historical member closure",
  );
  for (let index = 0; index < record.members.length; index += 1) {
    if (record.members[index].path !== patch.member) {
      check(
        isDeepStrictEqual(document.member_inventory_before[index], record.members[index]),
        "historical preservation evidence",
      );
    }
  }
  verifyMetadataPreservation(document, record, patch);
  const oldMember = document.member_inventory_before.filter(
    (member) => member.path === patch.member,
  );
  const newMember = record.members.filter((member) => member.path === patch.member);
  check(
    oldMember.length === 1 &&
      newMember.length === 1 &&
      oldMember[0].kind === "file" &&
      newMember[0].kind === "file" &&
      oldMember[0].sha256 === patch.member_sha256_before &&
      newMember[0].sha256 === patch.member_sha256_after &&
      oldMember[0].bytes === patch.member_bytes_before &&
      newMember[0].bytes === patch.member_bytes_after &&
      patch.member_sha256_before !== patch.member_sha256_after,
    "patch member binding",
  );
}

function verifyMetadataPreservation(document, record, patch) {
  const keys = [
    "path",
    "type_hex",
    "mode",
    "bytes",
    "uid",
    "gid",
    "mtime",
    "uname_sha256",
    "gname_sha256",
    "linkname_sha256",
    "pax_headers_sha256",
    "pax_non_size_headers_sha256",
    "pax_size",
  ];
  const beforeMembers = new Map(
    document.member_inventory_before.map((member) => [member.path, member]),
  );
  const afterMembers = new Map(record.members.map((member) => [member.path, member]));
  const seen = new Set();
  const sizeDelta = patch.replacement_length - patch.removed_length;
  // The member inventory is sorted, but these records preserve original TAR order.
  for (let index = 0; index < document.member_metadata_before.length; index += 1) {
    const before = document.member_metadata_before[index];
    const after = document.member_metadata_after[index];
    exactKeys(before, keys);
    exactKeys(after, keys);
    check(typeof before.path === "string" && before.path === after.path, "metadata path/order");
    const path =
      before.type_hex === "35" && before.path.endsWith("/")
        ? before.path.slice(0, -1)
        : before.path;
    safePath(path);
    check(
      !seen.has(path) && beforeMembers.has(path) && afterMembers.has(path),
      "metadata member closure",
    );
    seen.add(path);
    for (const [metadata, member] of [
      [before, beforeMembers.get(path)],
      [after, afterMembers.get(path)],
    ]) {
      check(
        (member.kind === "file"
          ? ["00", "30"].includes(metadata.type_hex)
          : metadata.type_hex === "35") &&
          metadata.mode === member.archive_mode &&
          metadata.bytes === member.bytes &&
          Number.isSafeInteger(metadata.uid) &&
          Number.isSafeInteger(metadata.gid) &&
          Number.isFinite(metadata.mtime),
        "metadata inventory binding",
      );
      for (const field of [
        "uname_sha256",
        "gname_sha256",
        "linkname_sha256",
        "pax_headers_sha256",
        "pax_non_size_headers_sha256",
      ]) {
        check(HASH.test(metadata[field]), "metadata hash");
      }
      check(
        metadata.pax_size === null ||
          (integer(metadata.pax_size) && metadata.pax_size === metadata.bytes),
        "metadata PAX size binding",
      );
    }
    const expected = { ...before };
    if (path === patch.member) {
      expected.bytes += sizeDelta;
      if (sizeDelta !== 0 && expected.pax_size !== null) {
        expected.pax_size += sizeDelta;
        // Only the declared size header changes; the non-size header hash stays exact.
        expected.pax_headers_sha256 = after.pax_headers_sha256;
      }
    }
    check(isDeepStrictEqual(expected, after), "historical metadata preservation evidence");
  }
  check(seen.size === record.member_count, "metadata member closure");
}

class Inputs {
  constructor(root) {
    this.root = root;
    this.files = new Map();
    this.directories = new Map();
  }
  async file(path, maximum, keep = false) {
    safePath(path);
    check(!this.files.has(path), "duplicate input");
    const file = await readBounded(resolve(this.root, path), maximum, keep);
    this.files.set(path, { ...file, maximum });
    return file;
  }
  async json(path) {
    const file = await this.file(path, JSON_LIMIT, true);
    return { ...file, ...parseJson(file.contents) };
  }
  async directory(path, expected) {
    const allowed = new Map([["", new Set()]]);
    for (const name of expected) {
      safePath(name);
      const parts = name.split("/");
      for (let index = 0; index < parts.length; index += 1) {
        const parent = parts.slice(0, index).join("/");
        if (!allowed.has(parent)) allowed.set(parent, new Set());
        allowed.get(parent).add(parts[index]);
      }
    }
    for (const [parent, names] of allowed) {
      const absolute = resolve(this.root, path, parent);
      const stamp = await checkedPath(absolute, true);
      const actual = (await readdir(absolute)).sort();
      check(isDeepStrictEqual(actual, [...names].sort()), "undeclared directory input");
      this.directories.set(absolute, { stamp, names: actual });
    }
  }
  async unchanged() {
    for (const [absolute, directory] of this.directories) {
      check(
        sameStamp(directory.stamp, await checkedPath(absolute, true)) &&
          isDeepStrictEqual(directory.names, (await readdir(absolute)).sort()),
        "directory changed",
      );
    }
    for (const [path, file] of this.files) {
      const current = await readBounded(resolve(this.root, path), file.maximum);
      check(
        sameStamp(file.stamp, current.stamp) &&
          file.bytes === current.bytes &&
          file.sha256 === current.sha256 &&
          file.sha512 === current.sha512,
        "input changed",
      );
    }
  }
}

async function checkedPath(path, directory) {
  const root = parse(path).root;
  let current = root;
  for (const part of path.slice(root.length).split(sep).filter(Boolean)) {
    current = resolve(current, part);
    const metadata = await lstat(current, { bigint: true });
    check(
      !metadata.isSymbolicLink() &&
        (current === path && !directory ? metadata.isFile() : metadata.isDirectory()),
      "linked/nonregular path",
    );
  }
  const final = await lstat(path, { bigint: true });
  check(
    !final.isSymbolicLink() && (directory ? final.isDirectory() : final.isFile()),
    "path root type",
  );
  return final;
}

async function readBounded(path, maximum, keep = false) {
  const before = await checkedPath(path, false);
  check(before.size > 0n && before.size <= BigInt(maximum), "input byte bound");
  const descriptor = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  const hashes = Object.fromEntries(
    Object.keys(STRENGTH).map((algorithm) => [algorithm, createHash(algorithm)]),
  );
  const chunks = [];
  let bytes = 0;
  const chunk = Buffer.alloc(64 * 1024);
  try {
    const opened = await descriptor.stat({ bigint: true });
    check(sameObject(before, opened), "opened input differs");
    while (true) {
      const count = (
        await descriptor.read(chunk, 0, Math.min(chunk.length, maximum + 1 - bytes), null)
      ).bytesRead;
      if (count === 0) break;
      bytes += count;
      check(bytes <= maximum, "input grew beyond bound");
      for (const digest of Object.values(hashes)) digest.update(chunk.subarray(0, count));
      if (keep) chunks.push(Buffer.from(chunk.subarray(0, count)));
    }
    const after = await descriptor.stat({ bigint: true });
    check(sameStamp(opened, after) && BigInt(bytes) === before.size, "input changed during read");
  } finally {
    await descriptor.close();
  }
  const stamp = await checkedPath(path, false);
  check(sameStamp(before, stamp), "input path changed");
  const digests = Object.fromEntries(
    Object.entries(hashes).map(([algorithm, digest]) => [algorithm, digest.digest("hex")]),
  );
  return {
    bytes,
    sha256: digests.sha256,
    sha512: digests.sha512,
    hashes: digests,
    stamp,
    ...(keep ? { contents: Buffer.concat(chunks, bytes) } : {}),
  };
}

function parseJson(bytes) {
  const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  const document = JSON.parse(text);
  const tokens =
    text.match(
      /"(?:[^"\\]|\\.)*"|[{}[\]:,]|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?|true|false|null/gu,
    ) ?? [];
  let cursor = 0;
  const numberKinds = [];
  function visit(path, depth) {
    check(depth <= 128, "JSON nesting");
    const token = tokens[cursor++];
    if (token === "{") {
      const names = new Set();
      if (tokens[cursor] === "}") {
        cursor += 1;
        return;
      }
      while (true) {
        const name = JSON.parse(tokens[cursor++]);
        check(
          typeof name === "string" && !names.has(name) && tokens[cursor++] === ":",
          "duplicate JSON key",
        );
        names.add(name);
        visit(path + "/" + JSON.stringify(name), depth + 1);
        const delimiter = tokens[cursor++];
        if (delimiter === "}") break;
        check(delimiter === ",", "JSON object");
      }
    } else if (token === "[") {
      if (tokens[cursor] === "]") {
        cursor += 1;
        return;
      }
      let index = 0;
      while (true) {
        visit(path + "/" + index++, depth + 1);
        const delimiter = tokens[cursor++];
        if (delimiter === "]") break;
        check(delimiter === ",", "JSON array");
      }
    } else if (/^-?[0-9]/u.test(token)) {
      check(Number.isFinite(Number(token)), "finite JSON number");
      const integral = !/[.eE]/u.test(token);
      check(!integral || Number.isSafeInteger(Number(token)), "unsafe JSON integer");
      numberKinds.push([path, integral ? "integer" : "float"]);
    }
  }
  visit("", 0);
  check(cursor === tokens.length, "JSON framing");
  numberKinds.sort((left, right) => compare(left[0], right[0]));
  return { document, numberKinds };
}

function memberInventory(members) {
  check(Array.isArray(members), "member inventory");
  let previous = "";
  const aliases = new Map();
  for (const member of members) {
    exactKeys(member, [
      "path",
      "kind",
      "bytes",
      "archive_mode",
      ...(member.kind === "file" ? ["sha256"] : []),
    ]);
    safePath(member.path);
    addAliases(member.path, aliases);
    check(
      member.path > previous &&
        integer(member.bytes) &&
        integer(member.archive_mode) &&
        (member.kind === "file"
          ? HASH.test(member.sha256)
          : member.kind === "directory" && member.bytes === 0),
      "member inventory identity",
    );
    previous = member.path;
  }
}

function parseSri(value) {
  check(typeof value === "string" && value.trim().length > 0, "SRI missing");
  const tokens = [...new Set(value.trim().split(/\s+/u))].sort();
  let strongest = "sha1";
  for (const token of tokens) {
    const match = /^(sha1|sha256|sha384|sha512)-([A-Za-z0-9+/]+={0,2})$/u.exec(token);
    check(match !== null, "SRI syntax");
    const bytes = Buffer.from(match[2], "base64");
    check(
      bytes.length === { sha1: 20, sha256: 32, sha384: 48, sha512: 64 }[match[1]] &&
        bytes.toString("base64") === match[2],
      "SRI canonical encoding",
    );
    if (STRENGTH[match[1]] > STRENGTH[strongest]) strongest = match[1];
  }
  check(STRENGTH[strongest] >= STRENGTH.sha256, "weak-only SRI");
  return { tokens, strongest };
}
function verifySri(value, hashes) {
  const parsed = parseSri(value);
  check(
    parsed.tokens.some(
      (token) =>
        token ===
        parsed.strongest + "-" + Buffer.from(hashes[parsed.strongest], "hex").toString("base64"),
    ),
    "SRI differs",
  );
  return parsed.strongest;
}
function canonicalJson(value) {
  if (Array.isArray(value)) return "[" + value.map(canonicalJson).join(",") + "]";
  if (object(value))
    return (
      "{" +
      Object.keys(value)
        .sort()
        .map((key) => canonicalJson(key) + ":" + canonicalJson(value[key]))
        .join(",") +
      "}"
    );
  const encoded = JSON.stringify(value);
  return encoded.replace(
    /[\u007f-\uffff]/g,
    (character) => "\\u" + character.charCodeAt(0).toString(16).padStart(4, "0"),
  );
}
function safePath(path) {
  check(
    typeof path === "string" &&
      path.length > 0 &&
      !/[<>:"\\|?*\uD800-\uDFFF]/u.test(path) &&
      !containsAsciiControl(path) &&
      path
        .split("/")
        .every(
          (part) =>
            part !== "" &&
            part !== "." &&
            part !== ".." &&
            !/[. ]$/u.test(part) &&
            !/^(?:CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[1-9¹²³]|LPT[1-9¹²³])(?:\.|$)/iu.test(part),
        ),
    "unsafe portable path",
  );
}
function containsAsciiControl(value) {
  for (const character of value) {
    if (character.charCodeAt(0) < 32 || character === "\u007f") return true;
  }
  return false;
}
function addAliases(path, aliases) {
  const parts = path.split("/");
  for (let end = 1; end <= parts.length; end += 1) {
    const prefix = parts.slice(0, end).join("/");
    const folded = prefix.normalize("NFC").toUpperCase().toLowerCase();
    check(!aliases.has(folded) || aliases.get(folded) === prefix, "aliased path");
    aliases.set(folded, prefix);
  }
}
function packageName(name) {
  check(typeof name === "string", "package name");
  const parts = name.split("/");
  check(
    name.startsWith("@")
      ? parts.length === 2 && NAME.test(parts[0].slice(1)) && NAME.test(parts[1])
      : parts.length === 1 && NAME.test(name),
    "package name",
  );
}
function exactKeys(value, keys) {
  check(
    object(value) && isDeepStrictEqual(Object.keys(value).sort(), [...keys].sort()),
    "unexpected fields",
  );
}
function identityKey(value) {
  packageName(value.name);
  check(VERSION.test(value.version), "version");
  return value.name + "\n" + value.version;
}
function object(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function integer(value, positive = false) {
  return Number.isSafeInteger(value) && value >= (positive ? 1 : 0);
}
function sameObject(left, right) {
  return (
    left.dev === right.dev &&
    left.ino === right.ino &&
    left.mode === right.mode &&
    left.size === right.size &&
    left.mtimeNs === right.mtimeNs
  );
}
function sameStamp(left, right) {
  return sameObject(left, right) && left.ctimeNs === right.ctimeNs;
}
function compare(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}
function sri512(hex) {
  return "sha512-" + Buffer.from(hex, "hex").toString("base64");
}
function hash(algorithm, bytes) {
  return createHash(algorithm).update(bytes).digest("hex");
}
function check(condition, message) {
  if (!condition) throw new Error(message);
}

// Exercises descriptor-bound release reads with real files and deterministic
// path replacement, including platforms without a usable O_NOFOLLOW flag.

import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { constants, promises as fs } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { readRegularFile } from "./read-regular-file.mjs";

test("regular reads enforce exact inclusive bounds and reject non-files", async (context) => {
  const root = await fs.mkdtemp(join(tmpdir(), "rootlight-bounded-input-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  const path = join(root, "input");
  await fs.writeFile(path, "abc");
  assert.equal((await readRegularFile(path, 3)).toString(), "abc");
  await assert.rejects(readRegularFile(path, 2), /bounded regular file/u);
  await assert.rejects(readRegularFile(root, 3), /bounded regular file/u);
  await assert.rejects(readRegularFile(join(root, "missing"), 3), { code: "ENOENT" });
  await assert.rejects(readRegularFile(path, -1), /byte limit/u);
  await fs.writeFile(path, "");
  assert.equal((await readRegularFile(path, 0)).length, 0);
});

test("replacing the path after open is rejected before reading and closes the descriptor", async (context) => {
  const root = await fs.mkdtemp(join(tmpdir(), "rootlight-replaced-input-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  const path = join(root, "input");
  const replacement = join(root, "replacement");
  await fs.writeFile(path, "old");
  await fs.writeFile(replacement, "new");
  const originalOpen = fs.open;
  let opened;
  let reads = 0;
  context.mock.method(fs, "open", async (...arguments_) => {
    opened = await originalOpen(...arguments_);
    context.mock.method(opened, "read", () => {
      reads += 1;
      throw new Error("unvalidated descriptor must not be read");
    });
    await fs.rename(path, join(root, "original"));
    await fs.rename(replacement, path);
    return opened;
  });
  syncBuiltinESMExports();
  try {
    await assert.rejects(readRegularFile(path, 3), /identity changed/u);
    assert.equal(reads, 0);
    assert.equal(opened.fd, -1);
  } finally {
    context.mock.restoreAll();
    syncBuiltinESMExports();
  }
});

test("replacement after identity validation cannot redirect descriptor reads", async (context) => {
  const root = await fs.mkdtemp(join(tmpdir(), "rootlight-validated-input-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  const path = join(root, "input");
  const replacement = join(root, "replacement");
  await fs.writeFile(path, "old");
  await fs.writeFile(replacement, "new");
  const originalLstat = fs.lstat;
  context.mock.method(fs, "lstat", async (...arguments_) => {
    const metadata = await originalLstat(...arguments_);
    await fs.rename(path, join(root, "original"));
    await fs.rename(replacement, path);
    return metadata;
  });
  syncBuiltinESMExports();
  try {
    assert.equal((await readRegularFile(path, 3)).toString(), "old");
    assert.equal(await fs.readFile(path, "utf8"), "new");
  } finally {
    context.mock.restoreAll();
    syncBuiltinESMExports();
  }
});

test("symbolic links never qualify as release inputs", async (context) => {
  const root = await fs.mkdtemp(join(tmpdir(), "rootlight-linked-input-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  const path = join(root, "input");
  const link = join(root, "link");
  await fs.writeFile(path, "secret");
  try {
    await fs.symlink(path, link);
  } catch (error) {
    if (process.platform === "win32" && error.code === "EPERM") {
      context.skip("Windows account cannot create symbolic links");
      return;
    }
    throw error;
  }
  await assert.rejects(readRegularFile(link, 1024), /bounded regular file/u);
});

test("post-open path validation rejects symlinks without O_NOFOLLOW before reading", async (context) => {
  const root = await fs.mkdtemp(join(tmpdir(), "rootlight-followed-input-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  const path = join(root, "input");
  const link = join(root, "link");
  await fs.writeFile(path, "secret");
  try {
    await fs.symlink(path, link);
  } catch (error) {
    if (process.platform === "win32" && error.code === "EPERM") {
      context.skip("Windows account cannot create symbolic links");
      return;
    }
    throw error;
  }
  const originalOpen = fs.open;
  let opened;
  let reads = 0;
  context.mock.method(fs, "open", async (file, flags) => {
    opened = await originalOpen(file, flags & ~(constants.O_NOFOLLOW ?? 0));
    context.mock.method(opened, "read", () => {
      reads += 1;
      throw new Error("linked descriptor must not be read");
    });
    return opened;
  });
  syncBuiltinESMExports();
  try {
    await assert.rejects(readRegularFile(link, 1024), /bounded regular file/u);
    assert.equal(reads, 0);
    assert.equal(opened.fd, -1);
  } finally {
    context.mock.restoreAll();
    syncBuiltinESMExports();
  }
});

test("oversized descriptors are rejected before allocation or reading and always close", async (context) => {
  const root = await fs.mkdtemp(join(tmpdir(), "rootlight-oversized-input-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  const path = join(root, "input");
  await fs.writeFile(path, "oversized");
  const originalOpen = fs.open;
  let opened;
  let reads = 0;
  context.mock.method(fs, "open", async (...arguments_) => {
    opened = await originalOpen(...arguments_);
    context.mock.method(opened, "read", () => {
      reads += 1;
      throw new Error("oversized descriptor must not be read");
    });
    return opened;
  });
  const allocations = context.mock.method(Buffer, "alloc", () => {
    throw new Error("oversized descriptor must not allocate its content");
  });
  syncBuiltinESMExports();
  try {
    await assert.rejects(readRegularFile(path, 3), /bounded regular file/u);
    assert.equal(allocations.mock.callCount(), 0);
    assert.equal(reads, 0);
    assert.equal(opened.fd, -1);
  } finally {
    context.mock.restoreAll();
    syncBuiltinESMExports();
  }
});

test(
  "a FIFO substituted at open is rejected without waiting for a writer",
  { skip: process.platform === "win32" },
  async (context) => {
    const root = await fs.mkdtemp(join(tmpdir(), "rootlight-fifo-input-"));
    context.after(() => fs.rm(root, { recursive: true, force: true }));
    const path = join(root, "input");
    const fifo = join(root, "fifo");
    await fs.writeFile(path, "old");
    const execute = promisify(execFile);
    await execute("mkfifo", [fifo], { timeout: 5_000 });
    // A separate process bounds the regression even if open incorrectly blocks.
    const child = `
      import assert from "node:assert/strict";
      import { promises as fs } from "node:fs";
      import { syncBuiltinESMExports } from "node:module";
      const { readRegularFile } = await import(process.argv[1]);
      const [path, fifo] = process.argv.slice(2);
      const originalOpen = fs.open;
      let opened;
      fs.open = async (...args) => {
        await fs.rename(path, path + ".original");
        await fs.rename(fifo, path);
        opened = await originalOpen(...args);
        return opened;
      };
      syncBuiltinESMExports();
      await assert.rejects(readRegularFile(path, 3), /bounded regular file/u);
      assert.equal(opened.fd, -1);
    `;
    await execute(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        child,
        new URL("./read-regular-file.mjs", import.meta.url).href,
        path,
        fifo,
      ],
      { timeout: 5_000 },
    );
  },
);

for (const replacement of ["a", "abcdef"]) {
  test(`size changes during a read are rejected and close the descriptor (${replacement.length})`, async (context) => {
    const root = await fs.mkdtemp(join(tmpdir(), "rootlight-resized-input-"));
    context.after(() => fs.rm(root, { recursive: true, force: true }));
    const path = join(root, "input");
    await fs.writeFile(path, "abc");
    const originalOpen = fs.open;
    let opened;
    context.mock.method(fs, "open", async (...arguments_) => {
      opened = await originalOpen(...arguments_);
      const originalRead = opened.read;
      context.mock.method(opened, "read", async (...readArguments) => {
        await fs.writeFile(path, replacement);
        return originalRead.apply(opened, readArguments);
      });
      return opened;
    });
    syncBuiltinESMExports();
    try {
      await assert.rejects(readRegularFile(path, 3), /size changed/u);
      assert.equal(opened.fd, -1);
    } finally {
      context.mock.restoreAll();
      syncBuiltinESMExports();
    }
  });
}

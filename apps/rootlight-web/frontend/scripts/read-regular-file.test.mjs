// Exercises descriptor-bound release reads with real files and deterministic
// path replacement, including platforms without a usable O_NOFOLLOW flag.

import assert from "node:assert/strict";
import { promises as fs } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { readRegularFile } from "./read-regular-file.mjs";

test("regular reads enforce exact inclusive bounds and reject non-files", async (context) => {
  const root = await fs.mkdtemp(join(tmpdir(), "rootlight-bounded-input-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  const path = join(root, "input");
  await fs.writeFile(path, "abc");
  assert.equal((await readRegularFile(path, 3)).toString(), "abc");
  await assert.rejects(readRegularFile(path, 2), /bounded regular file/u);
  await assert.rejects(readRegularFile(root, 3), /bounded regular file/u);
  await assert.rejects(readRegularFile(path, -1), /byte limit/u);
  await fs.writeFile(path, "");
  assert.equal((await readRegularFile(path, 0)).length, 0);
});

test("replacing a checked regular file before open is rejected", async (context) => {
  const root = await fs.mkdtemp(join(tmpdir(), "rootlight-replaced-input-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  const path = join(root, "input");
  const replacement = join(root, "replacement");
  await fs.writeFile(path, "old");
  await fs.writeFile(replacement, "new");
  const originalOpen = fs.open;
  context.mock.method(fs, "open", async (...arguments_) => {
    await fs.rename(path, join(root, "original"));
    await fs.rename(replacement, path);
    return originalOpen(...arguments_);
  });
  syncBuiltinESMExports();
  try {
    await assert.rejects(readRegularFile(path, 3), /identity changed/u);
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

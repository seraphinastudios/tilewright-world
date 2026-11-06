// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Loading the module with initializeSync(): a failure before an instance is installed can be
// retried, and a failure after, as when its start traps, stops the module.
// node:test runs each file in its own process, so this one starts with the
// module not loaded.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initialize, initializeSync, openPack } from "../index.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
// The valid pack is named by its digest, which changes with the compiler
// version, so its name comes from the manifest.
const manifest = JSON.parse(await readFile(new URL("manifest.json", fixtures), "utf8"));
const valid = await readFile(new URL(manifest.valid[0].file, fixtures));
const module = await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url));
// A WebAssembly header, then a section of an unknown kind.
const broken = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0xff, 0x00]);

/** Runs `run` while growing any WebAssembly table fails, as it can when memory runs out. */
async function withoutTableGrowth(run) {
  const grow = WebAssembly.Table.prototype.grow;
  WebAssembly.Table.prototype.grow = () => {
    throw new RangeError("WebAssembly.Table.grow(): failed to grow table");
  };
  try {
    await run();
  } finally {
    WebAssembly.Table.prototype.grow = grow;
  }
}

test("initializeSync() can be retried after a failed compilation, not after a failed start", async () => {
  assert.throws(() => initializeSync(broken), WebAssembly.CompileError);
  assert.throws(() => openPack(valid), /initialize/);
  await withoutTableGrowth(() => {
    assert.throws(() => initializeSync(module), RangeError);
  });
  assert.throws(() => initializeSync(module), /stopped/);
  await assert.rejects(initialize(module), /stopped/);
  assert.throws(() => openPack(valid), /stopped/);
});

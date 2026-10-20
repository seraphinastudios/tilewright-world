// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Loading the module with initialize(): a failure before an instance is installed can be
// retried, and a failure after, as when its start traps, stops the module.
// node:test runs each file in its own process, so this one starts with the
// module not loaded.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initialize, initializeSync, openPack } from "../index.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
const valid = await readFile(new URL("5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack", fixtures));
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

test("initialize() can be retried after a failed download or compilation, not after a failed start", async () => {
  await assert.rejects(initialize(broken), WebAssembly.CompileError);
  assert.throws(() => openPack(valid), /initialize/);
  // The instance's start fails once wasm-bindgen has installed it.
  await withoutTableGrowth(() => assert.rejects(initialize(module), RangeError));
  await assert.rejects(initialize(module), /stopped/);
  assert.throws(() => initializeSync(module), /stopped/);
  assert.throws(() => openPack(valid), /stopped/);
});

// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Closing a pack frees it in the module; if that fails, close() throws and the
// module stops. node:test runs each file in its own process, so stopping the module
// here leaves the other tests alone.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initializeSync, openPack, PackError } from "../index.js";
import { Reader } from "../wasm/tilewright_pack.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
// The valid pack is named by its digest, which changes with the compiler
// version, so its name comes from the manifest.
const manifest = JSON.parse(await readFile(new URL("manifest.json", fixtures), "utf8"));
const valid = await readFile(new URL(manifest.valid[0].file, fixtures));
initializeSync(await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url)));

test("a failure to free a pack is thrown by close() and stops the module", () => {
  const pack = openPack(valid);
  const { free } = Reader.prototype;
  Reader.prototype.free = () => {
    throw new RangeError("failed to free");
  };
  try {
    assert.throws(() => pack.close(), RangeError);
  } finally {
    Reader.prototype.free = free;
  }
  assert.equal(pack.closed, true);
  assert.throws(() => openPack(valid), /stopped/);
});

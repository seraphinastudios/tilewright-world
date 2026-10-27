// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// A host operation that fails during a read, such as growing the table of
// JavaScript values, aborts the WebAssembly call midway, so the module
// stops as it does after a trap. node:test runs each file in its own
// process, so stopping the module here leaves the other tests alone.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initializeSync, openPack, PackError } from "../index.js";
import { Reader } from "../wasm/tilewright_pack.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
const valid = await readFile(new URL("5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack", fixtures));
initializeSync(await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url)));

test("a rejected pack does not stop the module", () => {
  assert.throws(() => openPack(new Uint8Array()), PackError);
  assert.equal(openPack(valid).summary().planes, 2);
});

test("an exception other than a rejected pack stops the module", () => {
  const pack = openPack(valid);
  const { summary } = Reader.prototype;
  Reader.prototype.summary = () => {
    throw new RangeError("WebAssembly.Table.grow(): failed to grow table");
  };
  try {
    assert.throws(() => pack.summary(), RangeError);
  } finally {
    Reader.prototype.summary = summary;
  }
  assert.throws(() => pack.meta("format"), /stopped/);
  assert.throws(() => openPack(valid), /stopped/);
});

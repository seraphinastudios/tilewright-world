// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// A call into the module can throw any value, undefined included; whatever
// it is, the module stops. node:test runs each file in its own process.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initialize, initializeSync, openPack } from "../index.js";
import { Reader } from "../wasm/tilewright_pack.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
const valid = await readFile(new URL("5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack", fixtures));
const module = await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url));
initializeSync(module);

test("a call that throws undefined stops the module", async () => {
  const pack = openPack(valid);
  const { summary, free } = Reader.prototype;
  let freed = 0;
  Reader.prototype.summary = () => {
    throw undefined;
  };
  Reader.prototype.free = function () {
    freed += 1;
    return free.call(this);
  };
  try {
    assert.throws(() => pack.summary(), (thrown) => thrown === undefined);
    assert.throws(() => pack.meta("format"), (error) => /stopped/.test(error.message) && error.cause === undefined);
    assert.throws(() => openPack(valid), /stopped/);
    assert.throws(() => initializeSync(module), /stopped/);
    await assert.rejects(initialize(module), /stopped/);
    pack.close();
    assert.equal(freed, 0);
  } finally {
    Object.assign(Reader.prototype, { summary, free });
  }
});

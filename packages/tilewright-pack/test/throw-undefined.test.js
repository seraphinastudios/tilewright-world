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
// The valid pack is named by its digest, which changes with the compiler
// version, so its name comes from the manifest.
const manifest = JSON.parse(await readFile(new URL("manifest.json", fixtures), "utf8"));
const valid = await readFile(new URL(manifest.valid[0].file, fixtures));
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

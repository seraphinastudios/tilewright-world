// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Loading the module from another realm's objects, as a page with an
// iframe can. A compiled module comes from a vm context. Node has no other
// realm with a URL, so a URL moved off its prototype stands for one: like
// another realm's, it is no instance of this realm's URL, yet its brand
// getter, href, holds. Node's Response brand check is instanceof, so a
// Response of another realm cannot be made here; that case was checked by
// hand in Chrome.
// node:test runs each file in its own process, so this one starts with the
// module not loaded.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";
import { initialize, initializeSync, openPack, Pack } from "../index.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
const valid = await readFile(new URL("5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack", fixtures));
const module = await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url));

function foreignURL() {
  const url = new URL(`data:application/wasm;base64,${module.toString("base64")}`);
  Object.setPrototypeOf(url, Object.prototype);
  return url;
}

test("initialize() fetches a URL of another realm; initializeSync() refuses its compiled module", async () => {
  // wasm-bindgen's synchronous loader cannot take another realm's module.
  const compiled = vm.runInNewContext("new WebAssembly.Module(bytes)", { bytes: new Uint8Array(module) });
  assert.ok(!(compiled instanceof WebAssembly.Module));
  assert.throws(() => initializeSync(compiled), (error) => error instanceof TypeError && /this realm/.test(error.message));
  assert.throws(() => openPack(valid), /initialize/);
  // A URL a promise settles to is not fetched, as the types say.
  await assert.rejects(initialize(Promise.resolve(foreignURL())), TypeError);
  assert.throws(() => openPack(valid), /initialize/);
  await initialize(foreignURL());
  assert.ok(openPack(valid) instanceof Pack);
});

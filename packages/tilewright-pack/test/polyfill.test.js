// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Importing the package where Response is a polyfill whose prototype has
// no status getter, as whatwg-fetch's and jsdom's may not.
// node:test runs each file in its own process, so this one starts with the
// module not loaded.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
// The valid pack is named by its digest, which changes with the compiler
// version, so its name comes from the manifest.
const manifest = JSON.parse(await readFile(new URL("manifest.json", fixtures), "utf8"));
const valid = await readFile(new URL(manifest.valid[0].file, fixtures));
const module = await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url));

globalThis.Response = class Response {
  constructor() {
    this.status = 200;
  }
};

test("the package imports and loads with a polyfilled Response", async () => {
  const { initialize, openPack, Pack } = await import("../index.js");
  await initialize(module);
  assert.ok(openPack(valid) instanceof Pack);
});

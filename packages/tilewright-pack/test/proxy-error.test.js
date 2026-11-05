// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// An exception whose inspection itself throws, as a proxy's can, is no
// rejected pack: the module stops. node:test runs each file in its own
// process, so stopping the module here leaves the other tests alone.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initializeSync, openPack } from "../index.js";
import { Reader } from "../wasm/tilewright_pack.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
// The valid pack is named by its digest, which changes with the compiler
// version, so its name comes from the manifest.
const manifest = JSON.parse(await readFile(new URL("manifest.json", fixtures), "utf8"));
const valid = await readFile(new URL(manifest.valid[0].file, fixtures));
initializeSync(await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url)));

test("an exception that cannot be inspected stops the module", () => {
  const pack = openPack(valid);
  const hostile = new Proxy(new Error("a failed host operation"), {
    getOwnPropertyDescriptor() {
      throw new Error("inspection failed");
    },
  });
  const { summary } = Reader.prototype;
  Reader.prototype.summary = () => {
    throw hostile;
  };
  try {
    assert.throws(() => pack.summary(), (thrown) => thrown === hostile);
  } finally {
    Reader.prototype.summary = summary;
  }
  assert.throws(() => pack.meta("format"), /stopped/);
});

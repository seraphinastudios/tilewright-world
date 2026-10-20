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
const valid = await readFile(new URL("5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack", fixtures));
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

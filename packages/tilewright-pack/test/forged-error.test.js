// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Only an error the reader made, with its own name and code, is a rejected
// pack; one that inherits them, as after prototype pollution, stops the
// module like any other exception. node:test runs each file in its own process, so stopping the module
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

test("an error that only inherits a pack error's name and code stops the module", () => {
  const pack = openPack(valid);
  assert.throws(() => openPack(new Uint8Array()), PackError);
  const { summary } = Reader.prototype;
  const name = Object.getOwnPropertyDescriptor(Error.prototype, "name");
  Object.defineProperty(Error.prototype, "name", { ...name, value: "PackError" });
  Object.defineProperty(Object.prototype, "code", { value: "pack.io", configurable: true, writable: true });
  Reader.prototype.summary = () => {
    throw new Error("a failed host operation");
  };
  try {
    assert.throws(() => pack.summary(), (error) => !(error instanceof PackError));
  } finally {
    Reader.prototype.summary = summary;
    Object.defineProperty(Error.prototype, "name", name);
    delete Object.prototype.code;
  }
  assert.throws(() => pack.meta("format"), /stopped/);
});

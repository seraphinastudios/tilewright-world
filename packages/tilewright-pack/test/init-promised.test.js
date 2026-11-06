// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Loading the module from a promise that is not this realm's Promise, as a
// thenable is, settling to a Response. Node's Response brand check is
// instanceof, so a Response of another realm cannot be made here; that case
// was checked by hand in Chrome.
// node:test runs each file in its own process, so this one starts with the
// module not loaded.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initialize, openPack, Pack } from "../index.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
// The valid pack is named by its digest, which changes with the compiler
// version, so its name comes from the manifest.
const manifest = JSON.parse(await readFile(new URL("manifest.json", fixtures), "utf8"));
const valid = await readFile(new URL(manifest.valid[0].file, fixtures));
const module = await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url));

test("initialize() settles a thenable and loads the Response it settles to", async () => {
  const response = new Response(module, { headers: { "content-type": "application/wasm" } });
  const thenable = {
    then(resolve) {
      resolve(response);
    },
  };
  await initialize(thenable);
  assert.ok(openPack(valid) instanceof Pack);
});

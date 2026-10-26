// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Loading the module from a promise that is not this realm's Promise, as a
// thenable is, settling to a Response. Node's Response cannot be made to
// look like another realm's, so a browser covers that case.
// node:test runs each file in its own process, so this one starts with the
// module not loaded.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initialize, openPack, Pack } from "../index.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
const valid = await readFile(new URL("5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack", fixtures));
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

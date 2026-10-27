// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Loading the module from code a source runs, such as a getter, while it
// is being loaded: one instance is ever installed, so a pack can never
// read another's data through a second one.
// node:test runs each file in its own process, so this one starts with the
// module not loaded.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
const valid = await readFile(new URL("5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack", fixtures));
const module = await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url));

// Counts the instances wasm-bindgen creates, before the package captures
// anything.
let instances = 0;
const { instantiate, Instance } = WebAssembly;
WebAssembly.instantiate = function (...args) {
  instances += 1;
  return instantiate.apply(this, args);
};
WebAssembly.Instance = new Proxy(Instance, {
  construct(target, args, newTarget) {
    instances += 1;
    return Reflect.construct(target, args, newTarget);
  },
});
const { initialize, initializeSync, openPack, Pack } = await import("../index.js");

test("a load started from a source's own code joins the load under way", async () => {
  // initializeSync(): code run while wasm-bindgen inspects the source, as
  // a proxy's prototype trap runs, can start no other load. The proxy is
  // no buffer, so this load then fails, leaving the module unloaded.
  const calls = [];
  const proxy = new Proxy(new Uint8Array(module), {
    getPrototypeOf(target) {
      if (calls.length === 0) {
        calls.push(initialize(module).catch((error) => error));
        try {
          initializeSync(module);
        } catch (error) {
          calls.push(error);
        }
      }
      return Reflect.getPrototypeOf(target);
    },
  });
  assert.throws(() => initializeSync(proxy), TypeError);
  assert.match((await calls[0]).message, /initializeSync\(\) is loading/);
  assert.match(calls[1].message, /initializeSync\(\) is loading/);
  assert.equal(instances, 0);
  assert.throws(() => openPack(valid), /initialize/);

  // initialize(): a `then` getter of the source, read as it is
  // normalized, gets the load under way.
  const source = new Uint8Array(module);
  let inner;
  let innerSync;
  Object.defineProperty(source, "then", {
    get() {
      if (inner === undefined) {
        inner = initialize(new Uint8Array(module));
        try {
          initializeSync(module);
        } catch (error) {
          innerSync = error;
        }
      }
      return undefined;
    },
  });
  const outer = initialize(source);
  await outer;
  assert.equal(inner, outer);
  assert.match(innerSync.message, /initialize\(\) is loading/);
  assert.equal(instances, 1);
  assert.ok(openPack(valid) instanceof Pack);
});

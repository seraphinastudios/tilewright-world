// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// The JavaScript API around the reader: loading the module, the inputs it
// accepts and checks, limits, digests, errors and closing.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import v8 from "node:v8";
import vm from "node:vm";
import { initialize, initializeSync, openPack, Pack, PackError } from "../index.js";
import { Reader } from "../wasm/tilewright_pack.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
const validName = "5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2";
const valid = await readFile(new URL(`${validName}.twpack`, fixtures));
const module = await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url));

// Garbage collection on demand, to see what a collected pack does.
v8.setFlagsFromString("--expose-gc");
const collect = vm.runInNewContext("gc");

/** Collects garbage until `done` holds, letting finalizers run. */
async function collectUntil(done) {
  for (let attempt = 0; attempt < 50 && !done(); attempt += 1) {
    collect();
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  return done();
}

/** Counts calls of `name` on readers while `run` runs. */
async function counting(name, run) {
  const original = Reader.prototype[name];
  let calls = 0;
  Reader.prototype[name] = function (...args) {
    calls += 1;
    return original.apply(this, args);
  };
  try {
    await run(() => calls);
  } finally {
    Reader.prototype[name] = original;
  }
  return calls;
}

function fixture(name) {
  return readFile(new URL(name, fixtures));
}

function rejects(run, code) {
  assert.throws(run, (error) => {
    assert.ok(error instanceof PackError, `${error}`);
    assert.ok(error instanceof Error);
    assert.equal(error.name, "PackError");
    assert.equal(error.code, code);
    return true;
  });
}

// node:test runs each file in its own process, so this one starts with the
// module not loaded.
test("packs open only once the module is loaded, by one instance", async () => {
  assert.throws(() => openPack(valid), /initialize/);
  let release;
  const source = new Promise((resolve) => {
    release = resolve;
  });
  const loading = initialize(source);
  assert.equal(initialize(module), loading);
  // A second instance would replace the exports the first one's packs use.
  assert.throws(() => initializeSync(module), /loading/);
  assert.throws(() => openPack(valid), /initialize/);
  release(module);
  await loading;
  initializeSync(module);
  await initialize(module);
  assert.ok(openPack(valid) instanceof Pack);
});

test("bytes are accepted as any view of an array buffer, and copied", async () => {
  await initialize(module);
  const padded = new Uint8Array(valid.length + 16);
  padded.set(valid, 8);
  const views = [
    valid,
    new Uint8Array(valid),
    new Uint8Array(valid).buffer,
    new DataView(padded.buffer, 8, valid.length),
    padded.subarray(8, 8 + valid.length),
  ];
  for (const bytes of views) {
    const pack = openPack(bytes, { digest: validName });
    assert.equal(pack.digest(), validName);
    assert.equal(pack.verify().chunks, 5);
  }
  // Changing the caller's bytes afterwards does not reach the pack.
  const copy = new Uint8Array(valid);
  const pack = openPack(copy, { digest: validName });
  copy.fill(0);
  assert.equal(pack.verify().chunks, 5);
  for (const bytes of [undefined, null, "pack", [1, 2, 3], 7, { byteLength: 4 }]) {
    assert.throws(() => openPack(bytes), TypeError);
  }
  // Buffers and views from another realm, such as an iframe, are accepted.
  const foreign = vm.runInNewContext("new ArrayBuffer(length)", { length: valid.length });
  assert.equal(foreign instanceof ArrayBuffer, false);
  new Uint8Array(foreign).set(valid);
  assert.equal(openPack(foreign, { digest: validName }).verify().chunks, 5);
  const foreignView = vm.runInNewContext("new Uint8Array(buffer)", { buffer: foreign });
  assert.equal(openPack(foreignView).digest(), validName);
  const shared = new SharedArrayBuffer(valid.length);
  new Uint8Array(shared).set(valid);
  assert.equal(openPack(shared).digest(), validName);
});

test("the expected digest is checked by verification", async () => {
  await initialize(module);
  const wrong = openPack(valid, { digest: "0".repeat(64) });
  assert.equal(wrong.digest(), validName);
  rejects(() => wrong.verify(), "pack.digest");
  assert.equal(openPack(valid).verify().objects, 11);
  for (const digest of [validName.toUpperCase(), validName.slice(1), `${validName}0`, 7, null]) {
    assert.throws(() => openPack(valid, { digest }), TypeError);
  }
});

test("limits are enforced and checked", async () => {
  await initialize(module);
  rejects(() => openPack(valid, { maxPackBytes: valid.length - 1 }), "pack.too_large");
  const pack = openPack(valid, { maxPackBytes: BigInt(valid.length) });
  rejects(() => pack.verify({ maxPackBytes: valid.length - 1 }), "pack.too_large");
  rejects(() => pack.verify({ maxDecodedBytes: 1 }), "pack.decode_budget");
  // Verification never uses a higher limit than opening did.
  assert.equal(pack.verify({ maxPackBytes: 2n ** 64n - 1n, maxDecodedBytes: 2n ** 64n - 1n }).chunks, 5);
  // Past what the module addresses: never copied, rejected by size. The
  // buffer is allocated lazily, so this costs no memory.
  const huge = new Uint8Array(2 ** 32 + 1);
  rejects(() => openPack(huge), "pack.sqlite");
  rejects(() => openPack(huge, { maxPackBytes: 2 ** 32 }), "pack.too_large");
  // An option bag that is not an object is an error, not the defaults.
  for (const bag of [1024, 1024n, "limits", true, null]) {
    assert.throws(() => openPack(valid, bag), TypeError);
    assert.throws(() => pack.verify(bag), TypeError);
  }
  for (const limit of [-1, 1.5, 2 ** 53, -1n, 2n ** 64n, "1", null]) {
    assert.throws(() => openPack(valid, { maxPackBytes: limit }), TypeError);
    assert.throws(() => pack.verify({ maxDecodedBytes: limit }), TypeError);
  }
});

test("rejected packs and reads throw PackError with the stable code", async () => {
  await initialize(module);
  rejects(() => openPack(new Uint8Array()), "pack.schema");
  const notDatabase = await fixture("open-not-a-database.twpack");
  rejects(() => openPack(notDatabase), "pack.sqlite");
  const trailing = await fixture("open-trailing-byte.twpack");
  rejects(() => openPack(trailing), "pack.file_length");
  const records = openPack(await fixture("chunk-records.twpack"));
  rejects(() => records.chunk("vault", 0, 2, 0), "pack.chunk_record");
  assert.equal(records.chunk("vault", 0, 1, 0).objects[0].uid, "control.chest");
  const long = openPack(await fixture("string-long.twpack"));
  rejects(() => long.string(22), "pack.string");
});

test("arguments are checked before they reach the reader", async () => {
  await initialize(module);
  const pack = openPack(valid);
  for (const layer of [0.5, 2 ** 31, -(2 ** 31) - 1, 0n, "0", Number.NaN]) {
    assert.throws(() => pack.chunk("overworld", layer, 0, 0), TypeError);
    assert.throws(() => pack.chunkKeys("overworld", layer), TypeError);
  }
  for (const position of [0.5, 2 ** 53, Number.POSITIVE_INFINITY, 2n ** 63n, -(2n ** 63n) - 1n, "0"]) {
    assert.throws(() => pack.chunk("overworld", 0, position, 0), TypeError);
    assert.throws(() => pack.stairwellStep("overworld", 0, 0, position), TypeError);
  }
  for (const id of [-1, 2 ** 32, 1.5, 1n]) {
    assert.throws(() => pack.string(id), TypeError);
  }
  for (const name of [undefined, null, 1, {}]) {
    assert.throws(() => pack.plane(name), TypeError);
    assert.throws(() => pack.meta(name), TypeError);
    assert.throws(() => pack.object(name), TypeError);
  }
  assert.throws(() => pack.definitions(1), TypeError);
  // Positions past the format's range are valid lookups that find nothing.
  assert.equal(pack.chunk("overworld", 0, 2n ** 63n - 1n, -(2n ** 63n)), undefined);
  assert.equal(pack.chunk("overworld", 2 ** 31 - 1, 0, 0), undefined);
  assert.equal(pack.stairwellStep("overworld", 0, 2 ** 53 - 1, 0), undefined);
  assert.equal(pack.string(2 ** 32 - 1), undefined);
  assert.equal(pack.objectLocation("Not A Uid"), undefined);
  assert.throws(() => new Pack({}), TypeError);
});

test("integers past 2^53 are bigints, others numbers", async () => {
  await initialize(module);
  const field = openPack(await fixture("reference-field-integer.twpack")).chunk("vault", 0, 0, 0);
  const values = field.objects.flatMap((object) => object.fields).map((entry) => entry.value);
  assert.deepEqual(
    values.filter((value) => value.type === "integer").map((value) => value.value),
    [2n ** 53n],
  );
  const portal = openPack(await fixture("reference-portal-position.twpack")).chunk("vault", 0, 0, 0);
  const location = portal.objects.map((object) => object.portal).find((found) => found?.kind === "location");
  assert.equal(typeof location.x, "bigint");
  assert.equal(typeof location.y, "number");
  const chunk = openPack(valid).chunk("overworld", 0, 0, 0);
  assert.ok(chunk.palette instanceof Uint32Array);
  assert.ok(chunk.heights instanceof Int32Array);
  assert.ok(chunk.zones instanceof Uint16Array);
  assert.equal(chunk.heights.length, chunk.chunkSize ** 2);
  for (const grid of chunk.ground) {
    assert.ok(grid instanceof Uint16Array);
    assert.equal(grid.length, chunk.chunkSize ** 2);
  }
});

test("listings read a page at a time from the last key", async () => {
  await initialize(module);
  const pack = openPack(valid);
  const listings = [
    ["planes", () => pack.planes()],
    ["zones", () => pack.zones("overworld")],
    ["anchors", () => pack.anchors("overworld")],
    ["stairwells", () => pack.stairwells("overworld")],
    ["definitions", () => pack.definitions()],
    ["definitions", () => pack.definitions("proto")],
    ["layers", () => pack.layers("overworld")],
    ["chunkKeys", () => pack.chunkKeys("overworld", 0)],
  ];
  for (const [name, list] of listings) {
    const all = [...list()];
    assert.ok(all.length > 1, name);
    // With pages of one row, the walk resumes after each row's key.
    const read = Reader.prototype[name];
    const calls = [];
    Reader.prototype[name] = function (...args) {
      calls.push(args);
      args[args.length - 2] = 1;
      return read.apply(this, args);
    };
    try {
      assert.deepEqual([...list()], all, name);
    } finally {
      Reader.prototype[name] = read;
    }
    assert.equal(calls.length, all.length + 1, name);
  }
  // So does a byte budget the first row reaches.
  const definitions = Reader.prototype.definitions;
  let pages = 0;
  Reader.prototype.definitions = function (...args) {
    pages += 1;
    args[args.length - 1] = 1;
    return definitions.apply(this, args);
  };
  try {
    assert.equal([...pack.definitions()].length, 12);
  } finally {
    Reader.prototype.definitions = definitions;
  }
  assert.equal(pages, 13);
  // The walk keeps its place when the caller changes the rows it is given.
  const changed = [];
  for (const plane of pack.planes()) {
    changed.push(plane.id);
    plane.id = "";
  }
  assert.deepEqual(changed, ["overworld", "vault"]);
  // A listing is lazy: nothing is read until it is iterated, and a pack
  // closed meanwhile ends it with an error, even within a page read already.
  const planes = pack.planes();
  pack.close();
  assert.throws(() => planes.next(), /closed/);
  const reopened = openPack(valid);
  const walk = reopened.planes();
  assert.equal(walk.next().value.id, "overworld");
  reopened.close();
  assert.throws(() => walk.next(), /closed/);
});

test("chunk reads keep to their budget of objects and fields", async () => {
  await initialize(module);
  const pack = openPack(valid);
  const entries = (objects) =>
    objects.reduce((total, object) => total + 1 + object.fields.length + entries(object.inventory), 0);
  const count = entries(pack.chunk("overworld", 0, 0, 0).objects);
  assert.ok(count > 1);
  assert.equal(entries(pack.chunk("overworld", 0, 0, 0, { maxEntries: count }).objects), count);
  // Fields count as well as objects.
  rejects(() => pack.chunk("overworld", 0, 0, 0, { maxEntries: count - 1 }), "pack.decode_budget");
  assert.ok(pack.chunk("overworld", 1, 0, 0, { maxEntries: 0n }));
  const uid = "o.k3vq7zr2mxa5d6wfh4pnyt2cgu";
  assert.equal(pack.object(uid, { maxEntries: count }).object.uid, uid);
  rejects(() => pack.object(uid, { maxEntries: 0 }), "pack.decode_budget");
  for (const limits of [1, null, { maxEntries: -1 }, { maxEntries: 1.5 }, { maxEntries: "1" }]) {
    assert.throws(() => pack.chunk("overworld", 0, 0, 0, limits), TypeError);
    assert.throws(() => pack.object(uid, limits), TypeError);
  }
});

test("stairwell reads keep to their budget of steps", async () => {
  await initialize(module);
  const pack = openPack(valid);
  const stairwells = [...pack.stairwells("overworld")];
  const most = Math.max(...stairwells.map((stairwell) => stairwell.steps.length));
  const longest = stairwells.find((stairwell) => stairwell.steps.length === most);
  assert.ok(most >= 2);
  assert.equal(pack.stairwell("overworld", longest.id, { maxEntries: most }).steps.length, most);
  rejects(() => pack.stairwell("overworld", longest.id, { maxEntries: most - 1 }), "pack.decode_budget");
  assert.equal([...pack.stairwells("overworld", { maxEntries: most })].length, stairwells.length);
  rejects(() => [...pack.stairwells("overworld", { maxEntries: 1 })], "pack.decode_budget");
  for (const limits of [1, null, { maxEntries: -1 }]) {
    assert.throws(() => pack.stairwell("overworld", longest.id, limits), TypeError);
    assert.throws(() => pack.stairwells("overworld", limits), TypeError);
  }
});

test("a pack collected without being closed is freed", async () => {
  await initialize(module);
  const freed = await counting("free", async (calls) => {
    (() => {
      openPack(valid).summary();
    })();
    assert.ok(await collectUntil(() => calls() > 0), "the collected pack was freed");
  });
  assert.ok(freed > 0);
});

test("a closed pack refuses reads", async () => {
  await initialize(module);
  const pack = openPack(valid);
  assert.equal(pack.closed, false);
  pack.close();
  assert.equal(pack.closed, true);
  assert.throws(() => pack.summary(), /closed/);
  pack.close();
  const disposed = openPack(valid);
  if (typeof Symbol.dispose === "symbol") {
    disposed[Symbol.dispose]();
    assert.equal(disposed.closed, true);
  }
});

test("results and errors are own properties, whatever a page adds to prototypes", async () => {
  await initialize(module);
  const pack = openPack(valid);
  const expected = [...pack.planes()].map((plane) => plane.id);
  const chunk = pack.chunk("overworld", 0, 0, 0);
  const polluted = [
    [Object.prototype, "id"],
    [Object.prototype, "code"],
    [Object.prototype, "objects"],
    [Array.prototype, "0"],
  ];
  for (const [prototype, key] of polluted) {
    Object.defineProperty(prototype, key, { get: () => "polluted", set() {}, configurable: true });
  }
  try {
    assert.deepEqual(
      [...pack.planes()].map((plane) => plane.id),
      expected,
    );
    const again = pack.chunk("overworld", 0, 0, 0);
    assert.equal(again.objects.length, chunk.objects.length);
    assert.ok(Object.hasOwn(again.objects, "0"));
    assert.throws(() => openPack(new Uint8Array()), (error) => Object.hasOwn(error, "code") && error.code === "pack.schema");
  } finally {
    for (const [prototype, key] of polluted) {
      delete prototype[key];
    }
  }
});

// Last, since it stops the module for the rest of this process.
test("a trap stops the module for good", async () => {
  await initialize(module);
  const pack = openPack(valid);
  const { summary, free, __destroy_into_raw: detach } = Reader.prototype;
  let freed = 0;
  let detached = 0;
  Reader.prototype.summary = () => {
    throw new WebAssembly.RuntimeError("unreachable");
  };
  Reader.prototype.free = function () {
    freed += 1;
    return free.call(this);
  };
  Reader.prototype.__destroy_into_raw = function () {
    detached += 1;
    return detach.call(this);
  };
  try {
    // A pack still open when the module stops, then dropped.
    let dropped = openPack(valid);
    assert.throws(() => pack.summary(), WebAssembly.RuntimeError);
    assert.throws(() => pack.meta("format"), /stopped/);
    assert.throws(() => dropped.meta("format"), /stopped/);
    assert.throws(() => openPack(valid), /stopped/);
    dropped = undefined;
    // Neither closing a pack nor collecting one calls into the stopped
    // module: each reader is detached from wasm-bindgen's finalizer.
    pack.close();
    assert.equal(pack.closed, true);
    assert.equal(detached, 1);
    assert.ok(await collectUntil(() => detached >= 2), "the dropped pack was detached");
    assert.equal(freed, 0);
  } finally {
    Object.assign(Reader.prototype, { summary, free, __destroy_into_raw: detach });
  }
});

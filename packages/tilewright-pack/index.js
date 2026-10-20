// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// The Tilewright pack reader for the browser: the Rust reader of
// `tilewright-pack`, compiled to WebAssembly, behind a small API. The rules,
// bounds and error codes are the Rust reader's own (docs/spec/PACK.md).

import initialize_, { initSync, Reader } from "./wasm/tilewright_pack.js";

/** Why a pack was rejected, with its stable code such as `pack.chunk_record`. */
export class PackError extends Error {
  constructor(code, message) {
    super(message);
    own(this, "name", "PackError", false);
    own(this, "code", code, true);
  }
}

// Defines an own data property: unlike assignment, it finds no setter a
// page may have added to a prototype, and its descriptor inherits nothing.
function own(target, key, value, enumerable) {
  Object.defineProperty(target, key, { __proto__: null, value, writable: true, enumerable, configurable: true });
}

// The module is "unloaded", "loading" while initialize() waits for it, or
// "ready". One instance is ever installed: a second would replace the
// exports that open packs still call.
let state = "unloaded";
let loading;

/**
 * Loads the WebAssembly module once; later calls return the same promise.
 * Without `source` it is fetched from beside this file, which browsers and
 * bundlers that understand `new URL(..., import.meta.url)` resolve;
 * elsewhere, such as in Node, pass the module's bytes, a
 * `WebAssembly.Module`, a URL or a `Response`.
 */
export function initialize(source) {
  if (stopped !== undefined) {
    return Promise.reject(stoppedError());
  }
  if (state === "ready") {
    return Promise.resolve();
  }
  if (state === "unloaded") {
    state = "loading";
    loading = initialize_(source === undefined ? undefined : { module_or_path: source }).then(
      () => {
        state = "ready";
      },
      (error) => {
        failed(error);
        throw error;
      },
    );
  }
  return loading;
}

/**
 * Loads the WebAssembly module from its bytes or a compiled module,
 * synchronously; it does nothing once the module is loaded, and throws
 * while `initialize()` is loading it.
 */
export function initializeSync(source) {
  usable();
  if (state === "loading") {
    throw new Error("initialize() is loading the module; await it instead");
  }
  if (state === "unloaded") {
    try {
      initSync({ module: source });
    } catch (error) {
      failed(error);
      throw error;
    }
    state = "ready";
  }
}

/**
 * After loading failed: whether wasm-bindgen had installed the instance
 * before the failure, as it has when the instance's start traps. Its
 * initSync returns at once when an instance is installed, and otherwise
 * fails before doing anything on an undefined module.
 */
function installed() {
  try {
    initSync(undefined);
    return true;
  } catch {
    return false;
  }
}

/**
 * Loading failed. Before an instance was installed, as when a download or
 * compilation fails, loading can be tried again; after, the instance is
 * not whole, and the module stops.
 */
function failed(error) {
  if (installed()) {
    stop(error);
  } else {
    state = "unloaded";
    loading = undefined;
  }
}

function argument(condition, message) {
  if (!condition) {
    throw new TypeError(message);
  }
}

function text(value, name) {
  argument(typeof value === "string", `${name} must be a string`);
  return value;
}

const I32 = [-(2 ** 31), 2 ** 31 - 1];
const U32 = [0, 2 ** 32 - 1];
const I64 = [-(2n ** 63n), 2n ** 63n - 1n];
const U64 = [0n, 2n ** 64n - 1n];
const DEFAULT_MAX_PACK_BYTES = 8n * 2n ** 30n;

// What a listing reads at a time: at most 64 rows, and no more once their
// keys and documents reach 1 MiB, so a page of large documents is one row.
const PAGE = 64;
const PAGE_BYTES = 2 ** 20;

// wasm-bindgen converts numbers to 32-bit integers silently, so each is
// checked here: an integer number or bigint within the parameter's range.
function int(value, name, [low, high]) {
  argument(
    Number.isInteger(value) && value >= low && value <= high,
    `${name} must be an integer from ${low} to ${high}`,
  );
  return value;
}

function big(value, name, [low, high]) {
  let converted;
  if (typeof value === "bigint") {
    converted = value;
  } else {
    argument(Number.isSafeInteger(value), `${name} must be a safe integer or a bigint`);
    converted = BigInt(value);
  }
  argument(converted >= low && converted <= high, `${name} must be from ${low} to ${high}`);
  return converted;
}

function limit(value, name) {
  return value === undefined ? undefined : big(value, name, U64);
}

function options(value, name) {
  if (value === undefined) {
    return {};
  }
  argument(typeof value === "object" && value !== null, `${name} must be an object`);
  return value;
}

// Brand checks that hold for buffers from another realm, such as an iframe:
// the byteLength getter throws for anything else.
const bufferLengths = [ArrayBuffer, globalThis.SharedArrayBuffer]
  .filter((type) => typeof type === "function")
  .map((type) => Object.getOwnPropertyDescriptor(type.prototype, "byteLength").get);

function isBuffer(value) {
  return bufferLengths.some((length) => {
    try {
      length.call(value);
      return true;
    } catch {
      return false;
    }
  });
}

// A chunk read builds values for each object and field of the chunk, so it
// fails with pack.decode_budget past this many entries, objects with
// inventories included and fields, unless the caller sets another budget.
// A record of at most 16 MiB can hold about a million of either.
const DEFAULT_MAX_ENTRIES = 65536n;

function entryBudget(limits) {
  const { maxEntries } = options(limits, "limits");
  return maxEntries === undefined ? DEFAULT_MAX_ENTRIES : big(maxEntries, "limits.maxEntries", U64);
}

function bytesOf(bytes) {
  if (ArrayBuffer.isView(bytes)) {
    return new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }
  argument(isBuffer(bytes), "bytes must be an ArrayBuffer or a view of one, such as a Uint8Array");
  return new Uint8Array(bytes);
}

// A trap, such as running out of memory, stops the module in the middle of
// a call, so its state is no longer known: it is not used again.
let stopped;

// Frees the reader of a pack that is collected without being closed. The
// registry holds each reader until its pack is collected, so this, and not
// wasm-bindgen's own finalizer for the reader, always runs first; and once
// the module has stopped, it detaches the reader instead of calling in.
const unclosed =
  typeof FinalizationRegistry === "function" ? new FinalizationRegistry((reader) => release(reader)) : undefined;

function release(reader) {
  if (stopped !== undefined) {
    // Unregisters wasm-bindgen's finalizer without calling into the module.
    reader.__destroy_into_raw();
    return;
  }
  try {
    reader.free();
  } catch (error) {
    rethrown(error);
  }
}

function stop(error) {
  stopped = error;
}

function stoppedError() {
  return new Error("the pack reader stopped after a WebAssembly error; load the page or process again", {
    cause: stopped,
  });
}

function usable() {
  if (stopped !== undefined) {
    throw stoppedError();
  }
}

/**
 * Converts what the reader throws: a rejected pack becomes a `PackError`,
 * and a trap stops the module.
 */
function rethrown(error) {
  if (error instanceof WebAssembly.RuntimeError) {
    stop(error);
  } else if (error instanceof Error && error.name === "PackError" && typeof error.code === "string") {
    return new PackError(error.code, error.message);
  }
  return error;
}

/**
 * Opens a pack from its bytes, which are copied: later changes to `bytes`
 * do not reach the pack. `options.digest` is the SHA-256 the bytes must
 * have, as 64 lowercase hex digits, which `verify` checks;
 * `options.maxPackBytes` is the pack size limit, 8 GiB by default.
 */
export function openPack(bytes, settings) {
  usable();
  if (state !== "ready") {
    throw new Error("call initialize() or initializeSync() before openPack()");
  }
  const image = bytesOf(bytes);
  const { digest, maxPackBytes } = options(settings, "options");
  if (digest !== undefined) {
    argument(
      typeof digest === "string" && /^[0-9a-f]{64}$/.test(digest),
      "options.digest must be 64 lowercase hex digits",
    );
  }
  const maximum = limit(maxPackBytes, "options.maxPackBytes");
  // The module addresses at most 4 GiB, and its reader takes lengths as
  // 32-bit numbers; SQLite could not allocate such a pack anyway.
  if (image.byteLength > U32[1]) {
    if (BigInt(image.byteLength) > (maximum ?? DEFAULT_MAX_PACK_BYTES)) {
      throw new PackError("pack.too_large", `the pack is more than ${maximum ?? DEFAULT_MAX_PACK_BYTES} bytes, the pack size limit`);
    }
    throw new PackError("pack.sqlite", "the pack is larger than WebAssembly memory can hold");
  }
  let reader;
  try {
    reader = Reader.open(image, digest, maximum);
  } catch (error) {
    throw rethrown(error);
  }
  return new Pack(reader);
}

/** An open pack. Reads are synchronous and each checks what it loads. */
export class Pack {
  #reader;

  constructor(reader) {
    argument(reader instanceof Reader, "open a pack with openPack()");
    this.#reader = reader;
    unclosed?.register(this, reader, this);
  }

  /**
   * The rows of a listing, read a page at a time: `fetch` reads the page
   * after a key, and `keyOf` gives a row's key. Only one page is held, and
   * the reader's keys rise with every page, so the walk ends.
   */
  *#rows(fetch, keyOf) {
    let after;
    for (;;) {
      const page = this.#call((reader) => fetch(reader, after));
      if (page.length === 0) {
        return;
      }
      // The key is taken before the caller sees, and may change, the rows.
      after = keyOf(page[page.length - 1]);
      yield* page;
    }
  }

  #call(read) {
    if (this.#reader === undefined) {
      throw new Error("the pack is closed");
    }
    usable();
    try {
      return read(this.#reader);
    } catch (error) {
      throw rethrown(error);
    }
  }

  /** Whether `close` has released the pack. */
  get closed() {
    return this.#reader === undefined;
  }

  /** Releases the pack's memory now rather than when it is collected. */
  close() {
    const reader = this.#reader;
    this.#reader = undefined;
    if (reader !== undefined) {
      unclosed?.unregister(this);
      release(reader);
    }
  }

  summary() {
    return this.#call((reader) => reader.summary());
  }

  meta(key) {
    text(key, "key");
    return this.#call((reader) => reader.meta(key));
  }

  profile() {
    return this.#call((reader) => reader.profile());
  }

  layerHeight() {
    return this.#call((reader) => reader.layerHeight());
  }

  string(id) {
    int(id, "id", U32);
    return this.#call((reader) => reader.string(id));
  }

  plane(id) {
    text(id, "id");
    return this.#call((reader) => reader.plane(id));
  }

  planes() {
    return this.#rows(
      (reader, after) => reader.planes(after, PAGE, PAGE_BYTES),
      (plane) => plane.id,
    );
  }

  zone(plane, id) {
    text(plane, "plane");
    text(id, "id");
    return this.#call((reader) => reader.zone(plane, id));
  }

  zones(plane) {
    text(plane, "plane");
    return this.#rows(
      (reader, after) => reader.zones(plane, after, PAGE, PAGE_BYTES),
      (zone) => zone.id,
    );
  }

  anchor(plane, id) {
    text(plane, "plane");
    text(id, "id");
    return this.#call((reader) => reader.anchor(plane, id));
  }

  anchors(plane) {
    text(plane, "plane");
    return this.#rows(
      (reader, after) => reader.anchors(plane, after, PAGE, PAGE_BYTES),
      (anchor) => anchor.id,
    );
  }

  stairwell(plane, id) {
    text(plane, "plane");
    text(id, "id");
    return this.#call((reader) => reader.stairwell(plane, id));
  }

  stairwells(plane) {
    text(plane, "plane");
    return this.#rows(
      (reader, after) => reader.stairwells(plane, after, PAGE, PAGE_BYTES),
      (stairwell) => stairwell.id,
    );
  }

  stairwellStep(plane, layer, x, y) {
    text(plane, "plane");
    int(layer, "layer", I32);
    const [bigX, bigY] = [big(x, "x", I64), big(y, "y", I64)];
    return this.#call((reader) => reader.stairwellStep(plane, layer, bigX, bigY));
  }

  definition(type, id) {
    text(type, "type");
    text(id, "id");
    return this.#call((reader) => reader.definition(type, id));
  }

  definitions(type) {
    if (type !== undefined) {
      text(type, "type");
    }
    return this.#rows(
      (reader, after) => reader.definitions(type, after?.[0], after?.[1], PAGE, PAGE_BYTES),
      (definition) => [definition.type, definition.id],
    );
  }

  layers(plane) {
    text(plane, "plane");
    return this.#rows(
      (reader, after) => reader.layers(plane, after, PAGE, PAGE_BYTES),
      (layer) => layer,
    );
  }

  chunkKeys(plane, layer) {
    text(plane, "plane");
    int(layer, "layer", I32);
    return this.#rows(
      (reader, after) => reader.chunkKeys(plane, layer, after?.[0], after?.[1], PAGE, PAGE_BYTES),
      (key) => [BigInt(key.cx), BigInt(key.cy)],
    );
  }

  chunk(plane, layer, cx, cy, limits) {
    text(plane, "plane");
    int(layer, "layer", I32);
    const [bigX, bigY] = [big(cx, "cx", I64), big(cy, "cy", I64)];
    const maxEntries = entryBudget(limits);
    return this.#call((reader) => reader.chunk(plane, layer, bigX, bigY, maxEntries));
  }

  objectLocation(uid) {
    text(uid, "uid");
    return this.#call((reader) => reader.objectLocation(uid));
  }

  object(uid, limits) {
    text(uid, "uid");
    const maxEntries = entryBudget(limits);
    return this.#call((reader) => reader.object(uid, maxEntries));
  }

  verify(limits) {
    const { maxPackBytes: pack, maxDecodedBytes: decoded } = options(limits, "limits");
    const maxPackBytes = limit(pack, "limits.maxPackBytes");
    const maxDecodedBytes = limit(decoded, "limits.maxDecodedBytes");
    return this.#call((reader) => reader.verify(maxPackBytes, maxDecodedBytes));
  }

  digest() {
    return this.#call((reader) => reader.digest());
  }
}

// `using pack = openPack(...)` closes the pack where the runtime has
// explicit resource management.
if (typeof Symbol.dispose === "symbol") {
  Pack.prototype[Symbol.dispose] = Pack.prototype.close;
}

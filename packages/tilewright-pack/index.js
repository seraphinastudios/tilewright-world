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
    this.name = "PackError";
    this.code = code;
  }
}

let initialized = false;
let initializing;

/**
 * Loads the WebAssembly module once. Without `source` it is fetched from
 * beside this file, which browsers and bundlers that understand
 * `new URL(..., import.meta.url)` resolve; elsewhere, such as in Node, pass
 * the module's bytes, a `WebAssembly.Module`, a URL or a `Response`.
 */
export function initialize(source) {
  initializing ??= initialize_(source === undefined ? undefined : { module_or_path: source }).then(
    () => {
      initialized = true;
    },
    (error) => {
      initializing = undefined;
      throw error;
    },
  );
  return initializing;
}

/** Loads the WebAssembly module from its bytes or a compiled module, synchronously. */
export function initializeSync(source) {
  if (!initialized) {
    initSync({ module: source });
    initialized = true;
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

function bytesOf(bytes) {
  if (bytes instanceof Uint8Array) {
    return bytes;
  }
  if (bytes instanceof ArrayBuffer) {
    return new Uint8Array(bytes);
  }
  argument(ArrayBuffer.isView(bytes), "bytes must be a Uint8Array, an ArrayBuffer or a view of one");
  return new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}

// A trap, such as running out of memory, stops the module in the middle of
// a call, so its state is no longer known: it is not used again.
let stopped;

function usable() {
  if (stopped !== undefined) {
    throw new Error("the pack reader stopped after a WebAssembly error; load the page or process again", {
      cause: stopped,
    });
  }
}

/**
 * Converts what the reader throws: a rejected pack becomes a `PackError`,
 * and a trap stops the module.
 */
function rethrown(error) {
  if (error instanceof WebAssembly.RuntimeError) {
    stopped = error;
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
export function openPack(bytes, options = {}) {
  if (!initialized) {
    throw new Error("call initialize() or initializeSync() before openPack()");
  }
  usable();
  const image = bytesOf(bytes);
  const { digest, maxPackBytes } = options;
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
    if (stopped === undefined) {
      this.#reader?.free();
    }
    this.#reader = undefined;
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
    return this.#call((reader) => reader.planes());
  }

  zone(plane, id) {
    text(plane, "plane");
    text(id, "id");
    return this.#call((reader) => reader.zone(plane, id));
  }

  zones(plane) {
    text(plane, "plane");
    return this.#call((reader) => reader.zones(plane));
  }

  anchor(plane, id) {
    text(plane, "plane");
    text(id, "id");
    return this.#call((reader) => reader.anchor(plane, id));
  }

  anchors(plane) {
    text(plane, "plane");
    return this.#call((reader) => reader.anchors(plane));
  }

  stairwell(plane, id) {
    text(plane, "plane");
    text(id, "id");
    return this.#call((reader) => reader.stairwell(plane, id));
  }

  stairwells(plane) {
    text(plane, "plane");
    return this.#call((reader) => reader.stairwells(plane));
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
    return this.#call((reader) => reader.definitions(type));
  }

  layers(plane) {
    text(plane, "plane");
    return this.#call((reader) => reader.layers(plane));
  }

  chunkKeys(plane, layer) {
    text(plane, "plane");
    int(layer, "layer", I32);
    return this.#call((reader) => reader.chunkKeys(plane, layer));
  }

  chunk(plane, layer, cx, cy) {
    text(plane, "plane");
    int(layer, "layer", I32);
    const [bigX, bigY] = [big(cx, "cx", I64), big(cy, "cy", I64)];
    return this.#call((reader) => reader.chunk(plane, layer, bigX, bigY));
  }

  objectLocation(uid) {
    text(uid, "uid");
    return this.#call((reader) => reader.objectLocation(uid));
  }

  object(uid) {
    text(uid, "uid");
    return this.#call((reader) => reader.object(uid));
  }

  verify(limits = {}) {
    const maxPackBytes = limit(limits.maxPackBytes, "limits.maxPackBytes");
    const maxDecodedBytes = limit(limits.maxDecodedBytes, "limits.maxDecodedBytes");
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

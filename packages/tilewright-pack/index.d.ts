// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

/// <reference lib="esnext.disposable" />

/** Every code a rejected pack is reported with (docs/spec/PACK.md section 4). */
export type PackErrorCode =
  | "pack.io"
  | "pack.sqlite"
  | "pack.not_a_file"
  | "pack.too_large"
  | "pack.file_length"
  | "pack.schema"
  | "pack.identity"
  | "pack.free_pages"
  | "pack.meta"
  | "pack.profile"
  | "pack.string"
  | "pack.string_ids"
  | "pack.plane"
  | "pack.zone"
  | "pack.anchor"
  | "pack.stairwell"
  | "pack.stairwell_step"
  | "pack.definition"
  | "pack.chunk"
  | "pack.chunk_encoding"
  | "pack.chunk_data"
  | "pack.chunk_record"
  | "pack.reference"
  | "pack.object_uid"
  | "pack.integrity"
  | "pack.digest"
  | "pack.decode_budget";

/** Why a pack was rejected. Invalid arguments throw `TypeError` instead. */
export class PackError extends Error {
  readonly name: "PackError";
  readonly code: PackErrorCode;
  constructor(code: PackErrorCode, message: string);
}

/** What the WebAssembly module can be loaded from. */
export type ModuleSource =
  | BufferSource
  | WebAssembly.Module
  | URL
  | string
  | Request
  | Response
  | Promise<Response>;

/**
 * Loads the WebAssembly module once; later calls return the same promise.
 * Without `source` the module is fetched from beside the package's
 * `index.js`, which browsers and bundlers resolve. In Node, pass the bytes
 * of `@seraphinastudios/tilewright-pack/tilewright_pack.wasm`.
 */
export function initialize(source?: ModuleSource): Promise<void>;

/**
 * Loads the WebAssembly module from its bytes or a compiled module,
 * synchronously. It does nothing once the module is loaded, and throws
 * while `initialize()` is loading it.
 */
export function initializeSync(source: BufferSource | WebAssembly.Module): void;

/**
 * An integer the reader returns as a number when it is within
 * ±(2^53 − 1), and as a bigint otherwise. A pack that verifies has only
 * numbers: the format bounds every such value to ±(2^53 − 1), and
 * verification checks it.
 */
export type Integer = number | bigint;

/** An integer argument: a safe integer number, or a bigint. */
export type IntegerArgument = number | bigint;

export interface OpenOptions {
  /**
   * The SHA-256 the pack's bytes must have, as 64 lowercase hex digits.
   * `verify` checks it, as it checks a pack file named by its digest.
   */
  digest?: string;
  /** The pack size limit: 8 GiB by default. */
  maxPackBytes?: IntegerArgument;
}

export interface VerifyLimits {
  /** At most the limit the pack was opened with, which is the default. */
  maxPackBytes?: IntegerArgument;
  /** The decoded-byte budget for chunks: 8 GiB by default. */
  maxDecodedBytes?: IntegerArgument;
}

/** Row counts. */
export interface Summary {
  strings: number;
  planes: number;
  zones: number;
  anchors: number;
  stairwells: number;
  chunks: number;
  definitions: number;
  /** Objects in every chunk, inventories included; 0 unless from `verify`. */
  objects: number;
  /** The objects that have a uid. */
  objectUids: number;
}

export interface Plane {
  id: string;
  kind: "shared" | "instanced" | "per_player";
  chunkSize: number;
  /** The plane's source document as canonical JSON. */
  document: string;
}

export interface Zone {
  plane: string;
  id: string;
  /** The parent zone's ID, or null for a root zone. */
  parent: string | null;
  /** The zone's source entry as canonical JSON. */
  document: string;
}

export interface Anchor {
  plane: string;
  id: string;
  layer: number;
  x: number;
  y: number;
}

export interface StairwellStep {
  layer: number;
  x: number;
  y: number;
  /** The step's prototype ID. */
  proto: string;
}

export interface Stairwell {
  plane: string;
  id: string;
  /** At least two, on adjacent layers in one direction. */
  steps: StairwellStep[];
  direction: "both" | "down" | "up";
  hiddenOnOtherLayers: boolean;
  /** Canonical JSON of the stairwell's custom data, or null. */
  custom: string | null;
  /** The stairwell's source entry as canonical JSON. */
  document: string;
}

/** The stairwell step on a tile. */
export interface StairwellTile {
  /** The stairwell's ID. */
  stairwell: string;
  /** The step's index in the stairwell's steps. */
  step: number;
  proto: string;
}

export interface Definition {
  /** The definition type, such as `proto`. */
  type: string;
  id: string;
  /** The definition's source document as canonical JSON. */
  document: string;
}

/** The position of a chunk in its plane and layer. */
export interface ChunkKey {
  cx: number;
  cy: number;
}

/** Where the object with a uid is. */
export interface ObjectLocation {
  plane: string;
  layer: number;
  cx: number;
  cy: number;
  /** The object's index in its chunk record's depth-first order. */
  index: number;
}

/** A field value; `string` values are string IDs, `json` canonical JSON. */
export type FieldValue =
  | { type: "null"; value: null }
  | { type: "bool"; value: boolean }
  | { type: "integer"; value: Integer }
  | { type: "number"; value: number }
  | { type: "string"; value: number }
  | { type: "json"; value: string };

export interface Field {
  /** The field name's string ID. */
  name: number;
  value: FieldValue;
}

/** A portal; plane and anchor are string IDs, a plane of 0 being the same plane. */
export type Portal =
  | { kind: "location"; plane: number; layer: number; x: Integer; y: Integer }
  | { kind: "anchor"; plane: number; anchor: number };

/** An object of a chunk record. Strings are IDs into the pack's strings. */
export interface ChunkObject {
  uid: string | null;
  /** `y * chunkSize + x`, or null for an inventory entry. */
  tile: number | null;
  /** The prototype's string ID. */
  proto: number;
  /** Render layer index plus one, or 0 for the prototype's layer. */
  layer: number;
  subLayer: number;
  z: number;
  portal: Portal | null;
  fields: Field[];
  /** Canonical JSON of the object's custom data, or null. */
  custom: string | null;
  inventory: ChunkObject[];
}

/** A decoded chunk. Grids are row-major, `y` then `x`. */
export interface Chunk {
  chunkSize: number;
  /** String IDs of the values the grids use. */
  palette: Uint32Array;
  /** One grid per ground layer, in profile order: palette index + 1, 0 = empty. */
  ground: Uint16Array[];
  heights: Int32Array;
  /** Palette index + 1, or 0 for the plane's default zone. */
  zones: Uint16Array;
  objects: ChunkObject[];
}

/**
 * Opens a pack from its bytes, which are copied. Throws `PackError` if they
 * are not a pack of this format or are more than the pack size limit.
 */
export function openPack(bytes: ArrayBufferView | ArrayBufferLike, options?: OpenOptions): Pack;

/**
 * An open pack. Each read checks the shape and bounds of what it loads and
 * throws `PackError` with the code of the rule a stored value breaks; a
 * read of something the pack does not have returns `undefined`. `verify`
 * checks the whole pack, including that the targets reads name exist.
 *
 * Listings are iterators that read a page at a time, in key order: 64 rows,
 * or fewer once their keys and documents reach 1 MiB, so a walk over a
 * large table holds one page. A page is read when the iterator
 * reaches it, so closing the pack ends the walk with an error.
 */
export class Pack {
  private constructor();
  /** Whether `close` has released the pack; reads then throw. */
  readonly closed: boolean;
  /** Releases the pack's memory now rather than when it is collected. */
  close(): void;
  /**
   * `close`, for `using pack = openPack(...)`. It is defined where the
   * runtime has `Symbol.dispose`.
   */
  [Symbol.dispose](): void;

  /** Row counts; `objects` is 0. */
  summary(): Summary;
  meta(key: string): string | undefined;
  /** The stored profile as canonical JSON, checked as `verify` checks it. */
  profile(): string;
  /** Height units per layer, or undefined when the profile does not stack layers. */
  layerHeight(): number | undefined;
  string(id: number): string | undefined;

  plane(id: string): Plane | undefined;
  /** Every plane, in ID order. */
  planes(): IterableIterator<Plane>;
  zone(plane: string, id: string): Zone | undefined;
  /** The zones of a plane, in ID order. */
  zones(plane: string): IterableIterator<Zone>;
  anchor(plane: string, id: string): Anchor | undefined;
  /** The anchors of a plane, in ID order. */
  anchors(plane: string): IterableIterator<Anchor>;
  stairwell(plane: string, id: string): Stairwell | undefined;
  /** The stairwells of a plane, in ID order. */
  stairwells(plane: string): IterableIterator<Stairwell>;
  stairwellStep(
    plane: string,
    layer: number,
    x: IntegerArgument,
    y: IntegerArgument,
  ): StairwellTile | undefined;
  definition(type: string, id: string): Definition | undefined;
  /** The definitions of one type in ID order, or all by type and ID. */
  definitions(type?: string): IterableIterator<Definition>;

  /** The layers of a plane that have chunks, in order. */
  layers(plane: string): IterableIterator<number>;
  /** The positions of a layer's chunks, by cx and then cy. */
  chunkKeys(plane: string, layer: number): IterableIterator<ChunkKey>;
  chunk(plane: string, layer: number, cx: IntegerArgument, cy: IntegerArgument): Chunk | undefined;

  objectLocation(uid: string): ObjectLocation | undefined;
  /** The object with a uid, after checking that the object at its location has it. */
  object(uid: string): { location: ObjectLocation; object: ChunkObject } | undefined;

  /**
   * Checks the whole pack: SQLite integrity, the expected digest, every
   * row, document and chunk record, and every reference between them.
   * Returns the row counts, with `objects` counted.
   */
  verify(limits?: VerifyLimits): Summary;
  /** The SHA-256 of the pack's bytes, as 64 lowercase hex digits. */
  digest(): string;
}

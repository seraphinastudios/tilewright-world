// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// The shared reader conformance fixtures in fixtures/packs, run against the
// WebAssembly reader as the Rust and Go readers run them. Each pack is
// opened from its bytes, with the digest its name gives when it is one.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { initializeSync, openPack, PackError } from "../index.js";

const fixtures = new URL("../../../fixtures/packs/", import.meta.url);
initializeSync(await readFile(new URL("../wasm/tilewright_pack_bg.wasm", import.meta.url)));

// JSON numbers past 2^53 would lose precision here; the manifest has none.
const manifest = JSON.parse(await readFile(new URL("manifest.json", fixtures), "utf8"), (key, value) => {
  if (typeof value === "number" && Number.isInteger(value) && !Number.isSafeInteger(value)) {
    throw new Error(`manifest.json: ${key} is not a safe integer`);
  }
  return value;
});

/** The digest a file name gives, when it is 64 lowercase hex digits. */
function namedDigest(file) {
  return /^[0-9a-f]{64}\.twpack$/.test(file) ? file.slice(0, 64) : undefined;
}

async function open(file, options = {}) {
  const bytes = await readFile(new URL(file, fixtures));
  return openPack(bytes, { digest: namedDigest(file), ...options });
}

/** The code `run` fails with, or null when it succeeds. */
function outcome(run) {
  try {
    run();
    return null;
  } catch (error) {
    if (error instanceof PackError) {
      return error.code;
    }
    throw error;
  }
}

function read(pack, op) {
  switch (op.op) {
    case "chunk":
      return pack.chunk(op.plane, op.layer, op.cx, op.cy);
    case "string":
      return pack.string(op.id);
    case "stairwell":
      return pack.stairwell(op.plane, op.id);
    case "stairwell_step":
      return pack.stairwellStep(op.plane, op.layer, op.x, op.y);
    case "layer_height":
      return pack.layerHeight();
    case "profile":
      return pack.profile();
    case "object_location":
      return pack.objectLocation(op.uid);
    case "object":
      return pack.object(op.uid);
    case "plane":
      return pack.plane(op.id);
    case "zone":
      return pack.zone(op.plane, op.id);
    case "anchor":
      return pack.anchor(op.plane, op.id);
    case "definition":
      return pack.definition(op.type, op.id);
    default:
      throw new Error(`unknown read ${op.op}`);
  }
}

test("the manifest lists every code the types declare", async () => {
  assert.equal(manifest.format, "tilewright.pack-conformance/1");
  const types = await readFile(new URL("../index.d.ts", import.meta.url), "utf8");
  const declared = /export type PackErrorCode =([^;]*);/s.exec(types)[1];
  const codes = [...declared.matchAll(/"([a-z_.]+)"/g)].map((match) => match[1]);
  assert.deepEqual(codes, manifest.codes);
});

test("the WebAssembly reader conforms", async () => {
  const failures = [];
  for (const testCase of manifest.cases) {
    const check = (what, found, expected) => {
      if (found !== (expected ?? null)) {
        failures.push(`${testCase.file}: ${what}: ${found}, expected ${expected ?? null}`);
      }
    };
    let pack;
    try {
      pack = await open(testCase.file, { maxPackBytes: testCase.open_limits?.max_pack_bytes });
    } catch (error) {
      if (!(error instanceof PackError)) {
        throw error;
      }
      check(`open (${error.message})`, error.code, testCase.open);
      continue;
    }
    check("open", null, testCase.open);
    for (const op of testCase.reads ?? []) {
      check(JSON.stringify(op), outcome(() => read(pack, op)), op.error);
    }
    const limits = {
      maxPackBytes: testCase.verify_limits?.max_pack_bytes,
      maxDecodedBytes: testCase.verify_limits?.max_decoded_bytes,
    };
    check("verify", outcome(() => pack.verify(limits)), testCase.verify);
    pack.close();
  }
  assert.deepEqual(failures, []);
});

/** The rendering of fixtures/packs/README.md, whose SHA-256 the manifest records. */
function render(chunk) {
  const lines = [];
  const list = (values) => Array.from(values).join(" ");
  lines.push(`chunk_size ${chunk.chunkSize}`);
  lines.push(`palette ${list(chunk.palette)}`);
  for (const grid of chunk.ground) {
    lines.push(`ground ${list(grid)}`);
  }
  lines.push(`heights ${list(chunk.heights)}`);
  lines.push(`zones ${list(chunk.zones)}`);
  const bits = new DataView(new ArrayBuffer(8));
  const field = ({ type, value }) => {
    switch (type) {
      case "null":
        return "null";
      case "bool":
        return `bool ${value}`;
      case "integer":
        return `integer ${value}`;
      case "number":
        bits.setFloat64(0, value);
        return `number ${bits.getBigUint64(0).toString(16).padStart(16, "0")}`;
      case "string":
        return `string ${value}`;
      case "json":
        return `json ${value}`;
      default:
        throw new Error(`unknown field type ${type}`);
    }
  };
  const object = (record, depth) => {
    lines.push(
      `object ${depth} ${record.tile ?? 65535} ${record.proto} ${record.layer} ${record.subLayer} ${record.z}`,
    );
    if (record.uid !== null) {
      lines.push(`uid ${record.uid}`);
    }
    if (record.portal?.kind === "location") {
      const { plane, layer, x, y } = record.portal;
      lines.push(`portal location ${plane} ${layer} ${x} ${y}`);
    } else if (record.portal?.kind === "anchor") {
      lines.push(`portal anchor ${record.portal.plane} ${record.portal.anchor}`);
    }
    for (const { name, value } of record.fields) {
      lines.push(`field ${name} ${field(value)}`);
    }
    if (record.custom !== null) {
      lines.push(`custom ${record.custom}`);
    }
    for (const item of record.inventory) {
      object(item, depth + 1);
    }
  };
  for (const record of chunk.objects) {
    object(record, 0);
  }
  return lines.map((line) => `${line}\n`).join("");
}

function count(objects) {
  return objects.reduce((total, object) => total + 1 + count(object.inventory), 0);
}

test("the valid packs read back as the manifest lists", async () => {
  for (const valid of manifest.valid) {
    const pack = await open(valid.file);
    assert.equal(pack.digest(), namedDigest(valid.file));
    assert.deepEqual(pack.verify(), {
      strings: valid.summary.strings,
      planes: valid.summary.planes,
      zones: valid.summary.zones,
      anchors: valid.summary.anchors,
      stairwells: valid.summary.stairwells,
      chunks: valid.summary.chunks,
      definitions: valid.summary.definitions,
      objects: valid.summary.objects,
      objectUids: valid.summary.object_uids,
    });
    assert.deepEqual(pack.summary(), { ...pack.verify(), objects: 0 });
    for (const [key, value] of Object.entries(valid.meta)) {
      assert.equal(pack.meta(key), value, key);
    }
    assert.equal(pack.meta("missing"), undefined);
    assert.equal(pack.profile(), valid.meta.profile);
    assert.equal(pack.layerHeight(), valid.layer_height ?? undefined);
    valid.strings.forEach((value, index) => assert.equal(pack.string(index + 1), value));
    assert.equal(pack.string(valid.strings.length + 1), undefined);

    const planes = [...pack.planes()];
    assert.deepEqual(
      planes,
      valid.planes.map(({ id, kind, chunk_size: chunkSize, document }) => ({ id, kind, chunkSize, document })),
    );
    for (const plane of planes) {
      assert.deepEqual(pack.plane(plane.id), plane);
    }
    assert.equal(pack.plane("missing"), undefined);

    const zones = planes.flatMap((plane) => [...pack.zones(plane.id)]);
    assert.deepEqual(
      zones,
      valid.zones.map(({ plane, id, parent, document }) => ({ plane, id, parent: parent ?? null, document })),
    );
    for (const zone of zones) {
      assert.deepEqual(pack.zone(zone.plane, zone.id), zone);
    }

    const anchors = planes.flatMap((plane) => [...pack.anchors(plane.id)]);
    assert.deepEqual(anchors, valid.anchors);
    for (const anchor of anchors) {
      assert.deepEqual(pack.anchor(anchor.plane, anchor.id), anchor);
    }

    const stairwells = planes.flatMap((plane) => [...pack.stairwells(plane.id)]);
    assert.deepEqual(
      stairwells.map(({ plane, id, document }) => ({ plane, id, document })),
      valid.stairwells,
    );
    for (const stairwell of stairwells) {
      assert.deepEqual(pack.stairwell(stairwell.plane, stairwell.id), stairwell);
      const entry = JSON.parse(stairwell.document);
      assert.equal(stairwell.direction, entry.direction ?? "both");
      assert.equal(stairwell.steps.length, entry.steps.length);
    }
    for (const step of valid.steps) {
      assert.deepEqual(pack.stairwellStep(step.plane, step.layer, step.x, step.y), {
        stairwell: step.stairwell,
        step: step.step,
        proto: step.proto,
      });
    }

    const definitions = [...pack.definitions()];
    assert.deepEqual(definitions, valid.definitions);
    for (const definition of definitions) {
      assert.deepEqual(pack.definition(definition.type, definition.id), definition);
      assert.ok([...pack.definitions(definition.type)].some((listed) => listed.id === definition.id));
    }
    assert.deepEqual([...pack.definitions("missing")], []);

    for (const row of valid.object_uids) {
      const location = { plane: row.plane, layer: row.layer, cx: row.cx, cy: row.cy, index: row.object };
      assert.deepEqual(pack.objectLocation(row.uid), location);
      const found = pack.object(row.uid);
      assert.deepEqual(found.location, location);
      assert.equal(found.object.uid, row.uid);
    }
    assert.equal(pack.object("missing"), undefined);

    const keys = planes.flatMap((plane) =>
      [...pack.layers(plane.id)].flatMap((layer) =>
        [...pack.chunkKeys(plane.id, layer)].map(({ cx, cy }) => ({ plane: plane.id, layer, cx, cy })),
      ),
    );
    assert.deepEqual(
      keys,
      valid.chunks.map(({ plane, layer, cx, cy }) => ({ plane, layer, cx, cy })),
    );
    for (const expected of valid.chunks) {
      const chunk = pack.chunk(expected.plane, expected.layer, expected.cx, expected.cy);
      assert.deepEqual(
        Array.from(chunk.palette, (id) => pack.string(id)),
        expected.palette,
      );
      assert.equal(count(chunk.objects), expected.objects);
      const digest = createHash("sha256").update(render(chunk)).digest("hex");
      assert.equal(digest, expected.render_sha256, `${expected.plane} ${expected.layer} ${expected.cx} ${expected.cy}`);
    }
    assert.equal(pack.chunk("overworld", 0, 1000, 1000), undefined);
    pack.close();
  }
});

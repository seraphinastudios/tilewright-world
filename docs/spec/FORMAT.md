# Tilewright world format, version 1 (draft)

Status: draft. Format 1.0 is frozen once the first game migration and the
editor both build against it; until then, changes need a spec and fixture
update in the same pull request.

This document defines the authored source format for tile worlds. It is
game-neutral: a *game profile* supplies everything game-specific. The content
pack that programs load at runtime is defined in [PACK.md](PACK.md).

## 1. Model

Every tile has one address, `(plane, layer, x, y)`.

| Concept | Meaning |
| --- | --- |
| Plane | A continuous world: a tile grid that can grow in any direction. |
| Layer | An integer floor of a plane. `0` is the surface; negative layers are below, positive above. |
| Chunk | A fixed square of one layer, `chunk_size` × `chunk_size` tiles (32 in version 1). Storage, streaming and diff unit only: a chunk has no header and no links. |
| Object | An instance of a prototype defined by the profile, placed on a tile, with optional field overrides and an inventory. |
| Zone | A named area painted per tile and layer, with a parent. Zones carry properties such as display name, music or rules, as declared by the profile. |
| Portal | An object property that targets a location or an anchor. |
| Anchor | A stable named location referenced by scripts, quests, portals and respawn points. |

Chunk `(cx, cy)` covers tiles `x` in `[cx * chunk_size, (cx + 1) * chunk_size)`
and `y` in `[cy * chunk_size, (cy + 1) * chunk_size)`. Coordinates are signed
64-bit integers within the integer limits of section 3.

## 2. Source tree

```text
<root>/
  world.jsonc                       project manifest
  profile/profile.jsonc             game profile
  profile/...                       schemas, palettes and rules referenced by the profile
  planes/<plane>/plane.jsonc        plane definition
  planes/<plane>/zones.jsonc        zones of the plane
  planes/<plane>/anchors.jsonc      anchors of the plane
  planes/<plane>/layers/<layer>/<cx>_<cy>.jsonc   chunks
  defs/<type>/<id>.jsonc            definitions (prototypes, loot tables, ...)
```

Identifiers (`plane`, zone, anchor, definition and prototype IDs, render
layer names) match `^[a-z0-9][a-z0-9_.-]{0,127}$`. File names derive from
identifiers and coordinates; a file whose name disagrees with its content is
an error. Layer directories and chunk coordinates use canonical decimal
integers: no `+`, no leading zeros, no `-0`. Layers are 32-bit; chunk
coordinates are limited so that every tile coordinate stays within the
integer range of section 3.

Only the files above are read. Under `planes/` and `defs/`, any other file or
directory is an error, and so is a symbolic link anywhere in a path that is
read. Other files at the root, such as assets, scripts or a README, are
ignored. Assets such as images and sounds stay ordinary files referenced by
path or by an ID the profile defines.

Paths written inside documents (the profile path, schema paths) are relative,
`/`-separated, made of ASCII letters, digits, `_`, `.` and `-`, with no
component starting with `.` or `-`, and end in `.jsonc`. Schema paths are
relative to the profile's directory and cannot leave it.

## 3. Syntax: strict JSONC

Every source document is UTF-8 JSON with `//` line comments and `/* */` block
comments. Everything else follows RFC 8259, plus these rules:

- trailing commas, single-quoted strings, unquoted keys, hexadecimal numbers,
  leading `+`, `NaN` and infinities are errors;
- duplicate object keys are errors;
- a byte-order mark is an error;
- numbers must be finite; integers (no fraction or exponent) must be within
  ±9,007,199,254,740,991;
- decoded strings and keys may not contain NUL;
- documents are closed: a key not defined by this spec or by the profile's
  schema is an error, except below an explicit `"custom"` object whose keys
  are reviewed, domain-qualified namespaces.

Parsers enforce these limits while reading, not after building a tree:

| Limit | Value |
| --- | ---: |
| Encoded input | 64 MiB |
| Nesting depth | 64 |
| Values | 1,000,000 |
| Array items | 250,000 per array |
| Object keys | 256 per object |
| String or key | 1 MiB UTF-8 |
| Number lexeme | 128 bytes |
| Comments | 100,000 and 4 MiB total |
| Physical lines | 250,000 |
| Physical line | 1 MiB UTF-8 |

Tools preserve comments, key order, whitespace and untouched bytes. An edit
replaces the exact byte span of the values it changes.

## 4. Project manifest: `world.jsonc`

```jsonc
{
  "format": "tilewright.world/1",
  "profile": "profile/profile.jsonc",
  "planes": ["overworld", "creation"]
}
```

## 5. Game profile: `profile/profile.jsonc`

```jsonc
{
  "format": "tilewright.profile/1",
  "id": "example",
  "version": "1.0.0",
  "projection": {"kind": "isometric", "tile_width": 48, "tile_height": 24},
  // or {"kind": "top_down", "tile_size": 32}
  "render_layers": ["sys", "floor", "mask", "item", "item2", "wall", "living", "effect"],
  "ground_layers": ["floor", "mask"],
  "definition_types": {
    "proto": {"schema": "schema/proto.jsonc"}
  },
  "object_fields": "schema/object-fields.jsonc",
  "zone_properties": "schema/zone-properties.jsonc",
  "movement": {"max_step": 12, "fall_unit": 50}
}
```

- `projection` sets the default camera; consumers may switch per camera.
- `render_layers` orders draw layers (1 to 65,534 unique identifiers); an
  object's `layer` names one of them.
- `ground_layers` are the layers stored in terrain grids: at most 8, each a
  render layer, and never named `height` or `zone`.
- `definition_types` maps each definition type to its closed schema (section
  11). It must declare `proto`, the type objects and terrain cells refer to.
- `object_fields` and `zone_properties` name the schemas for object `fields`
  and zone `properties`.
- `movement` (optional) holds the editor's analysis rules, `max_step` and
  `fall_unit`; the format itself does not interpret them.
- `custom` (optional) holds namespaced extension data, as in section 7.

## 6. Plane: `planes/<plane>/plane.jsonc`

```jsonc
{
  "format": "tilewright.plane/1",
  "id": "overworld",
  "title": "Overworld",
  "kind": "shared",          // "shared", "instanced" or "per_player"
  "chunk_size": 32,
  "default_zone": "wilderness"
}
```

`title` (at most 256 characters) and `default_zone` are optional;
`default_zone` must be a zone of the plane. `chunk_size` must be 32 in format
1. `custom` is allowed.

## 7. Chunk: `planes/<plane>/layers/<layer>/<cx>_<cy>.jsonc`

```jsonc
{
  "format": "tilewright.chunk/1",
  "plane": "overworld",
  "layer": 0,
  "chunk": [3, 37],
  "terrain": {
    "floor": {
      "palette": {"g": "grass_1", "s": "sand_2"},
      "rows": ["ggsg...", "..."]      // chunk_size strings of chunk_size cells
    },
    "mask": {"palette": {"e": "grass_edge_n"}, "rows": ["..."]},
    "height": {"fill": 0},           // or "rows": [[0, 0, 4, ...], ...]
    "zone": {"palette": {"1": "island"}, "fill": "1"}
  },
  "objects": [
    {"at": [7, 0], "proto": "tree_1"},
    {"at": [15, 0], "proto": "wall_1", "layer": "wall", "fields": {"name": "Old wall"}},
    {"at": [9, 4], "proto": "stairs_down", "portal": {"layer": -1, "at": [105, 1188]}},
    {"at": [2, 2], "proto": "chest", "inventory": [{"proto": "coin", "fields": {"count": 3}}]}
  ]
}
```

Terrain grids:

- `terrain` holds one optional grid per profile ground layer, keyed by the
  layer's name (`floor` and `mask` above), plus optional `height` and `zone`.
  A missing ground grid is empty; missing heights are 0; a missing zone grid
  leaves every tile in the plane's default zone.
- A grid has either `"fill"` (one value for every tile) or `"rows"`.
- Character grids (ground layers and `zone`) map one palette key per tile.
  Keys are printable ASCII characters, not all `.`; `.` (or `..`) means
  empty. `"cell_width": 2` switches to two-character keys when a chunk needs
  more than 93 distinct values. Ground palette values are `proto`
  definitions; zone palette values are zones of the plane, each listed once.
- `height` rows are arrays of integers. Height is the ground elevation of the
  tile's terrain floor; objects may add their own `z`.
- A tile's terrain floor is the plain prototype in its cell. A floor with
  field overrides, or an additional floor on another sub-layer, is an object
  entry instead.

Objects:

| Key | Meaning |
| --- | --- |
| `at` | Chunk-local `[x, y]`; required on top-level objects, absent in inventories. |
| `proto` | Prototype ID; required. |
| `layer` | Render layer name; defaults to the prototype's layer. |
| `sub_layer` | Integer sub-layer, default 0. |
| `z` | Elevation offset, default 0. |
| `fields` | Field overrides, closed by the profile's `object_fields` and the prototype. |
| `portal` | `{"plane"?, "layer", "at": [x, y]}` in plane coordinates, or `{"plane"?, "anchor": id}`. Without `plane`, the target is this plane. |
| `inventory` | Ordered list of contained objects, nested at most 16 deep. |
| `custom` | Namespaced extension data: an object whose keys are domain-qualified namespaces such as `example.org` or `example.org/tool`. |

Object order is significant: objects on the same tile keep their listed order,
which is their stacking order within a layer and sub-layer. A multi-tile
object appears once, at its head tile; the prototype defines its footprint.

## 8. Zones: `planes/<plane>/zones.jsonc`

```jsonc
{
  "format": "tilewright.zones/1",
  "zones": [
    {"id": "island", "name": "Green Island", "properties": {"music": "calm"}},
    {"id": "mountains", "parent": "island", "name": "High Peaks"}
  ]
}
```

A tile's effective properties are its zone's properties over its ancestors'.
Its display name is the innermost zone that has a `name` (at most 256
characters). `properties` are checked against the profile's
`zone_properties` schema. Parents must be zones of the same plane, and parent
chains may not form cycles. `custom` is allowed on each zone.

## 9. Anchors: `planes/<plane>/anchors.jsonc`

```jsonc
{
  "format": "tilewright.anchors/1",
  "anchors": [
    {"id": "town.temple.altar", "layer": 0, "at": [118, 1612]}
  ]
}
```

## 10. Definitions: `defs/<type>/<id>.jsonc`

```jsonc
{
  "format": "tilewright.def/1",
  "type": "proto",
  "id": "tree_1",
  "fields": {"face": "tree_1.png", "layer": "item", "blocks_view": true}
}
```

`fields` are validated by the definition type's schema in the profile. The
file's directory must be the definition's `type` and its name the `id`.
`custom` is allowed.

## 11. Field schemas

Definition fields, object fields and zone properties are closed by schema
documents the profile names:

```jsonc
{
  "format": "tilewright.schema/1",
  "fields": {
    "face": {"type": "string", "required": true, "max_length": 128},
    "layer": {"type": "string", "values": ["floor", "item", "wall"]},
    "count": {"type": "integer", "min": 1, "max": 65535},
    "weight": {"type": "number", "min": 0},
    "blocks_view": {"type": "boolean"},
    "loot": {"type": "reference", "target": "loot"},
    "script": {"type": "json"}
  }
}
```

Field names match `^[a-z][a-z0-9_]{0,63}$`. Types:

| Type | Accepts | Options |
| --- | --- | --- |
| `boolean` | `true` or `false` | |
| `integer` | an integer | `min`, `max` |
| `number` | any number | `min`, `max` |
| `string` | a string | `max_length` (default 65,536 characters), `values` |
| `reference` | the ID of a definition | `target`: a definition type the profile declares |
| `json` | any value | |

`required` (default `false`) makes a field mandatory. A key that is not in
the schema is an error.

## 12. Validation summary

A source tree is valid when every document parses under section 3, every
document matches its section's shape and the profile's schemas, every
reference resolves (profile, schemas, planes, zones, anchors, prototypes,
references, portal targets), file names match content, and no two zones,
anchors, planes or definitions share an ID. Validators report every error
with file, byte span, line, column and a stable code:

| Code | Meaning |
| --- | --- |
| `jsonc.*` | Syntax and limit errors of section 3. |
| `source.io`, `source.encoding`, `source.budget` | Unreadable, non-UTF-8, or more files or bytes than the validator allows. |
| `source.symlink`, `source.unexpected_file`, `source.missing_file` | The file layout of section 2. |
| `source.path`, `source.name_mismatch` | A path in a document, or a file name that disagrees with its content. |
| `shape.format`, `shape.unknown_key`, `shape.missing_key`, `shape.type`, `shape.range`, `shape.identifier` | Document shape. |
| `ref.duplicate`, `ref.unresolved`, `ref.cycle` | Identity and references. |
| `grid.invalid` | Terrain grid structure. |

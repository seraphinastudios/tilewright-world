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

Identifiers (`plane`, zone, anchor, definition and prototype IDs) match
`^[a-z0-9][a-z0-9_.-]{0,127}$`. File names derive from identifiers and
coordinates; a file whose name disagrees with its content is an error.
Assets such as images and sounds stay ordinary files referenced by path or by
an ID the profile defines.

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
- `render_layers` orders draw layers; an object's `layer` names one of them.
- `ground_layers` are the layers stored in terrain grids.
- `definition_types` maps each definition type to its closed schema.
- `object_fields` and `zone_properties` declare the fields objects and zones
  may set, with types and bounds.
- `movement` holds the editor's analysis rules; the format itself does not
  interpret them.

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

- A grid has either `"fill"` (one value for every tile) or `"rows"`.
- Character grids (`floor`, `mask`, `zone`) map one palette key per tile.
  Keys are single printable ASCII characters other than `.`; `.` means empty.
  `"cell_width": 2` switches to two-character keys when a chunk needs more
  than 90 distinct values.
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
| `portal` | `{"plane"?, "layer", "at": [x, y]}` in plane coordinates, or `{"anchor": id}`. |
| `inventory` | Ordered list of contained objects. |
| `custom` | Namespaced extension data. |

Object order is significant: objects on the same tile keep their listed order,
which is their stacking order within a layer and sub-layer. A multi-tile
object appears once, at its head tile; the prototype defines its footprint.

## 8. Zones: `planes/<plane>/zones.jsonc`

```jsonc
{
  "format": "tilewright.zones/1",
  "zones": [
    {"id": "island", "name": "Strakewood Island", "properties": {"music": "th_16"}},
    {"id": "mountains", "parent": "island", "name": "Giant Mountains"}
  ]
}
```

A tile's effective properties are its zone's properties over its ancestors'.
Its display name is the innermost zone that has a `name`.

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

`fields` are validated by the definition type's schema in the profile.

## 11. Validation summary

A source tree is valid when every document parses under section 3, every
document matches its section's shape and the profile's schemas, every
reference resolves (profile, planes, zones, anchors, prototypes, portal
targets), file names match content, and no two objects claim the same
identity. Validators report every error with file, byte span and a stable
code.

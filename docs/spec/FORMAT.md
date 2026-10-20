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
| Layer | An integer floor of a plane. `0` is the surface; negative layers are below, positive above. With the profile's `layer_height`, layers stack vertically (section 5). |
| Chunk | A fixed square of one layer, `chunk_size` × `chunk_size` tiles (32 in version 1). Storage, streaming and diff unit only: a chunk has no header and no links. |
| Object | An instance of a prototype defined by the profile, placed on a tile, with optional field overrides, an inventory and a uid: a stable ID unique in the world. |
| Zone | A named area painted per tile and layer, with a parent. Zones carry properties such as display name, music or rules, as declared by the profile. |
| Portal | An object property that targets a location or an anchor. |
| Anchor | A stable named location referenced by scripts, quests, portals and respawn points. |
| Stairwell | One exit that spans several layers: a connected run of steps on adjacent layers, used from end to end in one move (section 10). |
| Terrain | A kind of ground a brush paints, such as grass or water. The profile's transitions derive the edge, corner and variant tiles where terrains meet (section 13). |

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
  planes/<plane>/stairwells.jsonc   stairwells of the plane
  planes/<plane>/layers/<layer>/<cx>_<cy>.jsonc   chunks
  defs/<type>/<id>.jsonc            definitions (prototypes, loot tables, ...)
```

Identifiers (`plane`, zone, anchor, stairwell, definition and prototype IDs,
render layer names) match `^[a-z0-9][a-z0-9_.-]{0,127}$`. File names derive from
identifiers and coordinates; a file whose name disagrees with its content is
an error. Layer directories and chunk coordinates use canonical decimal
integers: no `+`, no leading zeros, no `-0`. Layers are 32-bit; chunk
coordinates are limited so that every tile coordinate stays within the
integer range of section 3.

Only the files above are read. `planes/` and `defs/` are optional, but if
present they must be directories. Under them, any other file or directory is
an error, and so is a symbolic link anywhere in a path that is read. Other files at the root, such as assets, scripts or a README, are
ignored. Assets such as images and sounds stay ordinary files referenced by
path or by an ID the profile defines.

Paths written inside documents (the profile path, schema paths, the
transitions path) are relative, `/`-separated, made of ASCII letters,
digits, `_`, `.` and `-`, with no component starting with `.` or `-`, and
end in `.jsonc`. A path is at most 1,024 bytes and 32 components, each at
most 128 bytes. Schema and transitions paths are relative to the profile's
directory and cannot leave it. Several roles may name the same schema; it
is read once.

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
  are reviewed, domain-qualified namespaces. `custom` is allowed only where
  this spec says so (the profile, planes, objects, zones, stairwells and
  definitions), so that compiling never drops it.

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
replaces the exact byte span of the values it changes. Edits are made through
transactions ([TRANSACTIONS.md](TRANSACTIONS.md)).

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
  "layer_height": 50,
  "movement": {"max_step": 12, "fall_unit": 50},
  "transitions": "transitions.jsonc"
}
```

- `version` is a string of at most 64 characters.
- `projection` sets the default camera; consumers may switch per camera. Tile
  sizes are integers from 1 to 4,294,967,295.
- `render_layers` orders draw layers (1 to 65,534 unique identifiers); an
  object's `layer` names one of them.
- `ground_layers` are the layers stored in terrain grids: at most 8, each a
  render layer, and never named `height` or `zone`.
- `definition_types` maps each definition type to its closed schema (section
  12). It must declare `proto`, the type objects, terrain cells and stairwell
  steps refer to.
- `object_fields` and `zone_properties` name the schemas for object `fields`
  and zone `properties`.
- `layer_height` (optional, an integer from 1 to 65,535) is the height of one
  layer in height units. It stacks layers vertically for display and
  analysis: layer `L` sits at `L × layer_height`. Tile heights and object
  `z` stay relative to their layer, so a tile of layer `L` with height `h`
  is at elevation `L × layer_height + h`, and an object on it adds its `z`.
  The bound keeps every such elevation within the integer range of
  section 3. Without `layer_height`, layers are not stacked.
- `movement` (optional) holds the editor's analysis rules, `max_step` (0 to
  4,294,967,295) and `fall_unit` (1 to 4,294,967,295); the format itself does
  not interpret them.
- `transitions` (optional) names the terrain transitions document (section
  13), a path relative to the profile's directory, like a schema path.
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
    {"uid": "village.chest", "at": [2, 2], "proto": "chest", "inventory": [{"proto": "coin", "fields": {"count": 3}}]}
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
  Keys are visible ASCII characters (`!` through `~`; not space), not all
  `.`; `.` (or `..`) means empty. `"cell_width": 2` switches to
  two-character keys when a chunk needs more than 93 distinct values. Ground palette values are `proto`
  definitions; zone palette values are zones of the plane, each listed once.
- `height` rows are arrays of 32-bit signed integers (−2,147,483,648 to
  2,147,483,647). Height is the ground elevation of the tile's terrain floor,
  relative to its layer (section 5); objects may add their own `z`.
- A tile's terrain floor is the plain prototype in its cell. A floor with
  field overrides, or an additional floor on another sub-layer, is an object
  entry instead.
- With terrain transitions (section 13), the profile's terrain layer holds
  painted terrain, and the edge, corner and variant tiles around it are
  derived into the other ground layers, not stored.

Objects:

| Key | Meaning |
| --- | --- |
| `uid` | Optional stable ID of the object, unique in the world (below). |
| `at` | Chunk-local `[x, y]`; required on top-level objects, absent in inventories. |
| `proto` | Prototype ID; required. |
| `layer` | Render layer name; defaults to the prototype's layer. |
| `sub_layer` | 16-bit signed integer sub-layer (−32,768 to 32,767), default 0. |
| `z` | 32-bit signed elevation offset, default 0. |
| `fields` | Field overrides, closed by the profile's `object_fields` and the prototype. |
| `portal` | `{"plane"?, "layer", "at": [x, y]}` in plane coordinates, or `{"plane"?, "anchor": id}`. Without `plane`, the target is this plane. |
| `inventory` | Ordered list of contained objects, nested at most 16 deep. |
| `custom` | Namespaced extension data: an object whose keys are domain-qualified namespaces such as `example.org` or `example.org/tool`. |

Object order is significant: objects on the same tile keep their listed order,
which is their stacking order within a layer and sub-layer. A multi-tile
object appears once, at its head tile; the prototype defines its footprint.
An object's *index* in its chunk is its position in depth-first order: the
chunk's objects in list order, each followed by its inventory entries in
the same order, before the next object. In the example above, the chest is
index 3 and its coin index 4.

Object uids:

- `uid` names one object for good, so that saves, scripts and tools can
  refer to it whatever its tile, its place in a list or its container. It
  is optional and allowed on top-level objects and inventory entries
  alike. A multi-tile object is one entry, so it has one uid. Terrain cells
  and stairwell steps are not objects and have none.
- A uid is an identifier (section 2): 1 to 128 bytes. No identifier is
  reserved. A string that is not an identifier, such as one with a capital
  letter or a leading `_`, can never be a uid, so a game may name objects
  it creates at runtime that way without meeting an authored uid.
- Uids are unique in the whole world: no two objects share one, in any
  plane, layer, chunk or inventory. A uid names its object without naming a
  plane, and the object keeps it when it moves to another tile, chunk,
  plane or container. Each object that repeats a uid an object read before
  it already has is a `ref.duplicate` error.
- A uid is never reused in one world: once its object is removed, no other
  object is given it, since saves may still name it. A tool that copies an
  object gives the copy a new uid or none. One revision of a world cannot
  show reuse, so validators do not check it; `tilewright diff`
  ([DIFF.md](DIFF.md)) reports uids that a change removes.
- Tools that assign uids should make them independent of the object's
  place and of each other, such as 128 random bits in lowercase base32
  after a short prefix (`o.k3vq7zr2mxa5d6wfh4pnyt2cgu`), so that uids
  assigned on different branches do not collide. Authors may also write
  readable ones, such as `village.chest`.

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

## 10. Stairwells: `planes/<plane>/stairwells.jsonc`

```jsonc
{
  "format": "tilewright.stairwells/1",
  "stairwells": [
    {
      "id": "tower.stairs",
      "steps": [
        {"layer": 0, "at": [118, 1612], "proto": "stairs_up"},
        {"layer": 1, "at": [118, 1612], "proto": "stairwell"},
        {"layer": 2, "at": [118, 1612], "proto": "stairs_down"}
      ]
    },
    {
      "id": "cliff.path",
      "steps": [
        {"layer": 0, "at": [40, 12], "proto": "slope_top"},
        {"layer": -1, "at": [41, 12], "proto": "slope"},
        {"layer": -2, "at": [42, 13], "proto": "slope_foot"}
      ],
      "direction": "down",
      "hidden_on_other_layers": false
    }
  ]
}
```

| Key | Meaning |
| --- | --- |
| `id` | Stairwell ID, unique in the plane. |
| `steps` | Ordered list of at least two steps, each `{"layer", "at": [x, y], "proto"}`: a 32-bit layer, a position in plane coordinates and a prototype ID. |
| `direction` | `"both"` (default), `"down"` or `"up"`: which way the stairwell may be used. |
| `hidden_on_other_layers` | Boolean, default `true`: whether consumers draw only the step on the viewer's layer. |
| `custom` | Namespaced extension data, as in section 7. |

Steps:

- Each step after the first is on the layer next to the step before it, and
  all steps go the same way: layers rise by one at every step, or fall by
  one at every step. A step's `x` and `y` each differ from the step before
  it by at most 1, so a straight stairwell keeps its tile on every layer and
  a sloped one shifts up to one tile per layer.
- A step's `proto` is a `proto` definition: what is drawn for that segment.
  The stairwell places its steps; they are not chunk objects.
- A tile, `(layer, x, y)`, holds at most one step of the plane's stairwells.

Semantics:

- A stairwell is one exit. Its first and last steps are its ends; the lower
  end is the one on the lower layer. Using either end moves the user (a
  player, creature or anything else that moves) to the other end in one
  use, without stopping on the steps between. `direction` limits which end
  may be used: with `"down"` only the upper end, moving down; with `"up"`
  only the lower end, moving up; with `"both"` either.
- Pathfinding treats a stairwell as a single edge between its ends, directed
  as `direction` allows, never as a chain of edges through its steps. The
  steps between the ends are drawn, but they are not exits.
- Editors and clients show a stairwell as one continuous staircase. When
  `hidden_on_other_layers` is `true`, a consumer draws only the step on the
  layer the viewer is on; when it is `false`, it draws every step, stacked by
  `layer_height` where it stacks layers.
- What it takes to use an end, such as stepping onto its tile or an action,
  is the game's rule, as it is for portals.

## 11. Definitions: `defs/<type>/<id>.jsonc`

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

## 12. Field schemas

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
    "script": {"type": "json"},
    "tags": {"type": "list", "max_items": 8, "items": {"type": "string", "max_length": 32}}
  }
}
```

Field names match `^[a-z][a-z0-9_]{0,63}$`. Types:

| Type | Accepts | Options |
| --- | --- | --- |
| `boolean` | `true` or `false` | |
| `integer` | an integer | `min`, `max` |
| `number` | any number | `min`, `max` |
| `string` | a string | `max_length` (1 to 1,048,576 characters; default 65,536), `values` |
| `reference` | the ID of a definition | `target`: a definition type the profile declares |
| `json` | any value | |
| `list` | an array | `items` (required): the field spec every item matches, without `required`; `min_items`, `max_items` (0 to 250,000, the array limit of section 3; defaults 0 and 250,000; `min_items` at most `max_items`) |
| `object` | an object | `fields` (required): field specs by name, as in a schema document, each with an optional `required`; closed |

`required` (default `false`) makes a field mandatory; an object that lacks
required fields gets one diagnostic that names the first three and counts the
rest. A key that is not in the schema is an error. Checking an object costs
time in proportion to the fields it has, however many the schema requires, so
a schema with many required fields cannot multiply the work of many small
objects.

`list` and `object` types nest: a list's items and an object's fields may be
lists or objects in turn, at most 8 deep. A list or object field of a schema
is at level 1, a list or object inside it at level 2, and so on; a schema
with one at level 9 is an error (`shape.range`). Values are checked at every
level as top-level fields are: references resolve, numbers and strings keep
to their options, a key that an `object` type does not declare is an error,
and an object value that lacks required fields gets one diagnostic naming
them. A recipe and a weighted loot table:

```jsonc
// profile/schema/recipe.jsonc
{
  "format": "tilewright.schema/1",
  "fields": {
    "output": {"type": "reference", "target": "proto", "required": true},
    "ingredients": {
      "type": "list", "required": true, "min_items": 1,
      "items": {
        "type": "object",
        "fields": {
          "item": {"type": "reference", "target": "proto", "required": true},
          "count": {"type": "integer", "min": 1, "required": true}
        }
      }
    }
  }
}

// profile/schema/loot.jsonc: each roll picks one entry, with probability
// weight / total weight.
{
  "format": "tilewright.schema/1",
  "fields": {
    "entries": {
      "type": "list", "required": true, "min_items": 1,
      "items": {
        "type": "object",
        "fields": {
          "item": {"type": "reference", "target": "proto", "required": true},
          "weight": {"type": "integer", "min": 1, "required": true}
        }
      }
    }
  }
}
```

```jsonc
{"format": "tilewright.def/1", "type": "recipe", "id": "chest",
 "fields": {"output": "chest", "ingredients": [{"item": "plank", "count": 4}, {"item": "nail", "count": 8}]}}
```

The model and the content pack keep `list` and `object` values, like `json`
values, as canonical JSON.

## 13. Terrain transitions

A profile may name a transitions document. Its rules choose the edge,
corner and variant tiles around painted terrain: grass meeting dirt, a
shoreline, a random choice among grass tiles. The source then stores only
the terrain a brush paints, and every tool derives the same tiles from it.

```jsonc
// profile/transitions.jsonc
{
  "format": "tilewright.transitions/1",
  "layer": "floor",
  "terrains": [
    {"id": "water", "protos": ["water"], "transitions": [
      {"draw": "inside", "model": "blob", "layer": "mask",
       "tiles": {"255": [{"proto": "water", "weight": 3}, "water_ripple"], "0": "pond",
                 "124": "shore_n", "241": "shore_e", "199": "shore_s", "31": "shore_w"}}
    ]},
    {"id": "dirt", "protos": ["dirt"]},
    {"id": "grass", "protos": ["grass", "grass_flowers"], "transitions": [
      {"draw": "outside", "model": "corners", "layer": "mask", "with": ["dirt"], "seed": 7,
       "tiles": {"1": "grass_ne", "2": "grass_se", "3": "grass_e", "15": "grass_all"}}
    ]}
  ]
}
```

`layer` is the *terrain layer*: the ground layer that holds painted
terrain. `terrains` lists 1 to 1,024 terrains, from lowest to highest
precedence. Each has:

| Key | Meaning |
| --- | --- |
| `id` | Identifier, unique in the document. |
| `protos` | 1 to 1,024 `proto` definitions that are this terrain when they are in the terrain layer. A prototype belongs to at most one terrain. A brush that paints the terrain writes the first; the others let tiles placed by hand, such as a variant, count as the terrain. |
| `transitions` | Optional list of at most 8 transitions; a document has at most 256 in all. |

A tile's *terrain* is the terrain whose `protos` hold the tile's cell in the
terrain layer. A tile that is empty there, holds another prototype, or lies
in a chunk the plane does not have has no terrain. A transition:

| Key | Meaning |
| --- | --- |
| `draw` | `"inside"` draws on tiles of the transition's terrain, *T*, where its neighbours differ, such as a shore drawn inside the water. `"outside"` draws on tiles of the terrains in `with`, showing *T* reaching in from their neighbours, such as grass edges drawn over the dirt next to the grass. |
| `model` | The neighbourhood it reads: `"none"`, `"sides"`, `"corners"` or `"blob"` (below). An outside transition cannot use `"none"`. |
| `layer` | The ground layer its tiles go into: any but the terrain layer, which keeps the painted terrain. |
| `with` | Optional list of 1 to 1,024 unique terrain IDs, never *T*: the terrains it blends with. Without it, an inside transition blends with every other terrain, and an outside one draws onto every terrain listed before *T*, of which there must be at least one. |
| `seed` | Optional integer from 0 to 4,294,967,295, default 0, mixed into the choice of variants. |
| `tiles` | The tile of each pattern: an object whose keys are patterns, written as decimal integers without leading zeros. Each value is a prototype ID, or a list of 1 to 256 variants, each a prototype ID or `{"proto": id, "weight": w}` with `w` from 1 to 65,535 (default 1). Every prototype is a `proto` definition. |

Neighbours. The neighbours of tile `(x, y)` are the eight tiles around it
on its layer, across chunk edges: N `(x, y - 1)`, NE `(x + 1, y - 1)`, E
`(x + 1, y)`, SE `(x + 1, y + 1)`, S `(x, y + 1)`, SW `(x - 1, y + 1)`, W
`(x - 1, y)` and NW `(x - 1, y - 1)`. A neighbour *shows T* when, for an
inside transition, its terrain is not in `with` (it is *T*, a terrain *T*
does not blend with, or no terrain at all, so water continues under a
bridge and at the edge of the painted world); and, for an outside
transition, when its terrain is *T*.

Patterns. A model turns the neighbours into a pattern, a sum of bits:

| Model | Bits | Pattern |
| --- | --- | --- |
| `none` | | Always 0: every tile of *T* draws the same tile, as variants. |
| `sides` | N 1, E 2, S 4, W 8 | A side's bit is set when that neighbour shows *T*. 16 patterns. |
| `corners` | NE 1, SE 2, SW 4, NW 8 | Each corner touches two sides and the diagonal between them. Inside, its bit is set when all three show *T*; outside, when any does. The 16 patterns of a corner (Wang or marching-squares) set. |
| `blob` | N 1, NE 2, E 4, SE 8, S 16, SW 32, W 64, NW 128 | Side bits as in `sides`. Inside, a corner's bit is set when its diagonal and both its sides show *T*; outside, when its diagonal shows *T* and neither side does. 47 patterns each way; inside they are those of the common 47-tile blob set. |

The counts include pattern 0, where no neighbour shows *T*, which an
outside transition never draws. `tiles` may hold only patterns its model
produces for its `draw`, and never 0 when the transition is outside: an
inside table has at most 1 (`none`), 16 (`sides`, `corners`) or 47
(`blob`) keys, an outside table at most 15 or 46, and a blob table holds a
corner bit only with both its sides set (inside) or both clear (outside).
A pattern the table does not list draws nothing.

Resolution. The transitions of a document run in order: terrains in
order, and each terrain's transitions in order. For each tile with a
terrain, every transition that applies to it, an inside one of its terrain
or an outside one whose `with` holds its terrain, computes the tile's
pattern. If `tiles` lists the pattern, the transition picks a variant and
draws it into its `layer`, but only where the source leaves that cell
empty: a prototype the source places there wins, so an author can
override a transition by hand.

A later transition that draws into the same cell replaces the earlier
one's tile, so a terrain's edges draw over those of the terrains before it.
Transitions read only the painted terrain, never another transition's
tiles, so a tile's result does not depend on the order tiles are visited.
An outside transition draws only next to its own terrain, so the
transitions of a tile's terrain and of its neighbours' terrains are the
only ones that can draw on it: at most 72, however many the document has.

Variants. A pattern's tile is picked by a hash of the tile's position,
computed with 64-bit unsigned integers whose multiplications wrap:

```text
mix(z) = z ^= z >> 30; z *= 0xbf58476d1ce4e5b9; z ^= z >> 27; z *= 0x94d049bb133111eb; z ^ (z >> 31)
hash   = mix(mix(mix(mix(seed) ^ layer) ^ x) ^ y)
```

`layer`, `x` and `y` are the tile's layer and plane coordinates as 64-bit
two's complement. With *W* the sum of the variants' weights, the tile is
the first variant at which the running sum of weights exceeds `hash` mod
*W*. For example, `hash` is `0x7ab40e090f363a7d` for seed 0 at layer 0,
tile `(1, 0)`, and `0x2dc3a5ddf0d229e7` for seed 42 at layer -3, tile
`(100, -200)`. The same world always picks the same variants.

Where the tiles live. Source chunks store only painted terrain: a brush
that paints a terrain writes its first prototype into the terrain layer,
one cell per painted tile (a `terrain.set` operation,
[TRANSACTIONS.md](TRANSACTIONS.md)), so diffs and transactions stay as
small as the painting, and changing a rule never rewrites the world. The
compiler resolves transitions into the pack's other ground grids (PACK.md
section 2), so readers and renderers draw them without knowing the rules,
and the pack's terrain layer holds the painted terrain, for game logic
such as which tiles are water. The rules are not stored in the pack.
Editors and other tools call the same resolver (`Transitions::resolve_chunk`
in `tilewright-source`) to show the tiles. A tile's transitions depend on
its neighbours, so painting a tile changes what is drawn on the tiles
around it, in adjacent chunks too.

## 14. Validation summary

A source tree is valid when every document parses under section 3, every
document matches its section's shape and the profile's schemas, at every
level of `list` and `object` fields, every reference resolves (profile,
schemas, planes, zones, anchors, prototypes, references, portal targets,
stairwell steps, terrain transitions), file names match content, every stairwell's steps
connect, no tile holds two stairwell steps, no two zones, anchors,
stairwells, planes, definitions or terrains share an ID, no prototype
belongs to two terrains, and no two objects of the world share a uid. Validators report every error
with file, byte span, line, column and a stable code, up to a limit they
state (10,000 by default in the reference loader); past it they report one
`source.budget` diagnostic instead of the rest:

| Code | Meaning |
| --- | --- |
| `jsonc.*` | Syntax and limit errors of section 3. |
| `source.io`, `source.encoding`, `source.budget` | Unreadable, non-UTF-8, or more files, bytes or problems than the validator allows. |
| `source.symlink`, `source.unexpected_file`, `source.missing_file` | The file layout of section 2. |
| `source.path`, `source.name_mismatch` | A path in a document, or a file name that disagrees with its content. |
| `shape.format`, `shape.unknown_key`, `shape.missing_key`, `shape.type`, `shape.range`, `shape.identifier` | Document shape. |
| `ref.duplicate`, `ref.unresolved`, `ref.cycle` | Identity and references. |
| `grid.invalid` | Terrain grid structure. |
| `stairwell.step` | A stairwell step that is not on the layer next to the step before it, in the stairwell's direction, or is more than one tile from it on an axis. |
| `stairwell.overlap` | A tile that holds steps of two stairwells. |

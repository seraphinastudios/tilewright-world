# Tilewright content pack, version 1 (draft)

A content pack is the compiled, read-only form of a source tree
([FORMAT.md](FORMAT.md)). It is one SQLite database file named
`<sha256>.twpack`, where `<sha256>` is the lowercase hex SHA-256 of the file.
Game servers load it into memory at startup and may import it into
PostgreSQL; clients, bots and the browser (SQLite compiled to WebAssembly)
open it directly.

The compiler is deterministic: the same source tree and compiler version
always produce byte-identical packs. It builds the database in memory in one
transaction, inserting rows in primary-key order, runs `VACUUM`, verifies the
result as section 3 describes, and writes the serialized database. The file uses `page_size = 4096`,
`application_id = 0x54575031` (`"TWP1"`) and `user_version = 1`.

## 1. Tables

The schema is exactly these nine tables, with no indexes, views or triggers
beyond SQLite's automatic ones:

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE strings (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
CREATE TABLE planes (id TEXT PRIMARY KEY, kind TEXT NOT NULL, chunk_size INTEGER NOT NULL, document TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE zones (plane TEXT NOT NULL, id TEXT NOT NULL, parent TEXT, document TEXT NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID;
CREATE TABLE anchors (plane TEXT NOT NULL, id TEXT NOT NULL, layer INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID;
CREATE TABLE stairwells (plane TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID;
CREATE TABLE stairwell_steps (plane TEXT NOT NULL, layer INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL, stairwell TEXT NOT NULL, step INTEGER NOT NULL, proto TEXT NOT NULL, PRIMARY KEY (plane, layer, x, y)) WITHOUT ROWID;
CREATE TABLE chunks (plane TEXT NOT NULL, layer INTEGER NOT NULL, cx INTEGER NOT NULL, cy INTEGER NOT NULL, encoding TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (plane, layer, cx, cy)) WITHOUT ROWID;
CREATE TABLE definitions (type TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY (type, id)) WITHOUT ROWID;
```

`meta` holds exactly:

| Key | Value |
| --- | --- |
| `chunk_encoding` | `twc1+zstd` |
| `compiler` | The compiler's name and version, such as `tilewright-pack 0.1.0`. |
| `format` | `tilewright.pack/1` |
| `profile` | The profile document as canonical JSON, including its `layer_height`. |
| `profile_id`, `profile_version` | From the profile. |
| `source_digest` | Lowercase hex SHA-256 over every source file read: for each path in byte order, the UTF-8 relative path, a zero byte, the file length as little-endian `u64`, and the SHA-256 of the file. |

`document` columns hold the source document (or, for `zones` and
`stairwells`, the entry) as canonical JSON: comments and whitespace removed,
members in source order, numbers spelled exactly as in the source, strings
escaped only where JSON requires.

`stairwells` holds one row per stairwell: its plane and ID, and its entry in
`document`. `stairwell_steps` holds
one row per step, keyed by the step's tile so that a reader finds the step on
a tile, or the steps in an area of one layer, by primary key: the plane,
layer and plane coordinates, the stairwell's ID, the step's index in the
stairwell's `steps` from 0, and its prototype ID. The documents are
authoritative; `stairwell_steps` indexes exactly their steps.

`strings` interns every string that chunk records reference: prototype IDs,
ground and zone palette values, field names, string field values, and portal
plane and anchor IDs. IDs run from 1 in byte order of the values; 0 means
"none" wherever an ID is optional.

## 2. Chunk encoding `twc1+zstd`

`data` is a zstd frame (level 19, no dictionary, content size recorded)
containing a `twc1` record of at most 16 MiB. The frame itself is at most
16,842,752 bytes, zstd's compression bound for 16 MiB, so every encoded chunk
fits. All integers are little-endian.

```text
magic         4 bytes  "TWC1"
version       u16      1
chunk_size    u16      N, 1..=255
ground_count  u8       G, the profile's ground layer count (at most 8)
reserved      u8       0
palette_len   u32      P
palette       P × u32  string IDs, nonzero and strictly ascending: every value the grids use
ground        G × N² × u16  per ground layer in profile order: palette index + 1, 0 = empty
height        N² × i32
zone          N² × u16 palette index + 1, 0 = the plane's default zone
object_count  u32
objects       object_count × object
```

Grids are row-major, `y` then `x`. An object record:

```text
tile          u16      y * N + x; 0xFFFF in inventories
proto         u32      string ID
layer         u16      render layer index + 1, 0 = prototype default
sub_layer     i16
z             i32
flags         u8       bit 0 portal, bit 1 fields, bit 2 custom, bit 3 inventory; other bits 0
[portal]      u8 kind, then
              kind 0, location: u32 plane string ID (0 = same plane), i32 layer, i64 x, i64 y
              kind 1, anchor:   u32 plane string ID (0 = same plane), u32 anchor string ID
[fields]      u16 count (≥ 1), then count × (u32 name string ID, value)
[custom]      u32 byte length, canonical JSON of the object's custom data
[inventory]   u32 count (≥ 1), then count × object
```

A field value is a tag byte and payload: `0` null, `1` false, `2` true,
`3` i64, `4` finite f64, `5` string (u32 string ID), `6` JSON (u32 byte
length + canonical JSON) for arrays and objects, which includes every value
of a `list` or `object` field. `custom` is canonical JSON
for an object. Objects appear in source order; inventories nest at most 16
deep.

## 3. Reading

Packs are untrusted input. Readers check a value's type and size in SQL before
loading it, and never load a whole table into memory. Readers:

- open the file read-only and immutable, with SQLite's defensive mode on and
  untrusted schemas disabled;
- reject the file unless its schema is exactly section 1, its
  `application_id` is `"TWP1"`, its `user_version` is 1, and `meta` holds
  exactly the keys of section 1, as text of at most 64 MiB for `profile` and
  1 KiB for the others, names this format and encoding, and has a lowercase
  hex `source_digest`, its pages are 4096 bytes, and the file is exactly its
  pages: its length is `page_count × 4096`, with nothing, not even a sparse
  hole, after the last page;
- reject any chunk row whose `encoding` is not `twc1+zstd`, or whose `data` is
  not a blob within the bound of section 2, even when reading a single chunk;
- reject a string that is not text of at most 1 MiB, the source format's
  string limit, even when reading a single string;
- reject a stairwell whose document is not canonical, does not have the
  source format's shape or disagrees with its row, even when reading a
  single stairwell, and a step row whose stairwell or prototype is not an
  identifier, even when reading a single step;
- reject chunk records with an unknown version, flags or tags, out-of-range
  tiles, palette indexes or sizes, a palette that is not nonzero and strictly
  ascending, embedded JSON that is not canonical or not the required shape
  (custom data is an object whose keys are domain-qualified namespaces), data
  that is not exactly one zstd frame, a decompressed size above 16 MiB, or
  trailing bytes.

Decoding is linear in a pack's decoded size, which compression can make
thousands of times its file size. A full verification therefore has a
decoded-byte budget, which the caller sets and which is 8 GiB by default: the
sum of the decoded sizes the chunks' zstd frames record. It fails as soon as
the next chunk would pass the budget, before decompressing that chunk. The
`tilewright inspect` command sets the budget with `--max-decoded-bytes`.
Verification checks each chunk's objects as they decode instead of building
the chunk's object model, and keeps its target lookups in caches of bounded
size, so its memory stays bounded by one record.

A full verification also:

- runs SQLite's integrity check, and compares the file's SHA-256 (hashed in
  bounded blocks) with its name when the name is 64 hex digits, which must be
  the lowercase digest. The digest is computed from the file at the pack's
  path when the check runs, so it binds to the path, not to the file the
  database connection opened; packs are content-addressed files opened from
  directories the caller controls;
- checks that string IDs are exactly `1..=count` in byte order of their
  values, and every string is UTF-8 text of at most 1 MiB without NUL;
- checks that plane, zone, anchor, stairwell and definition keys are
  identifiers; plane kinds, and a chunk size of 32 as format 1 requires; zone
  planes, parents and that parent chains end at a root; anchor planes, layers
  and positions; stairwell planes; that every
  `stairwell_steps` row names a stairwell of its plane and a `proto`
  definition, with a 32-bit layer, a position within ±9,007,199,254,740,991
  and an index of at least 0; and chunk planes, layers and coordinates, which
  keep every tile within the source format's integer range;
- checks that every `document` column, and the `profile` meta value, is
  canonical JSON of an object of at most 64 MiB, whose `custom` data, if any,
  has domain-qualified namespace keys, and whose identity agrees with
  its row: `format`, `id`, `kind` and `chunk_size` for planes, `id` and
  `parent` for zones, `format`, `type` and `id` for definitions, and `format`,
  `id` and `version` against `profile_id` and `profile_version` for the
  profile, which lists 1 to 65,534 unique render layer identifiers and at
  most 8 unique ground layers, each a render layer other than `height` or
  `zone`, and has an `isometric` projection with `tile_width` and
  `tile_height` or a `top_down` one with `tile_size`, each from 1 to
  4,294,967,295, and a `layer_height`, if any, that is an integer from 1 to
  65,535; a plane's `default_zone`, when present, must be a zone of the plane;
- checks that each stairwell document has the source format's shape (section
  10 of FORMAT.md): an ID that matches its row, at least two steps whose
  layers change by one in one direction and whose positions move at most one
  tile per axis, and a valid direction and flag; that each step
  is the `stairwell_steps` row at its tile, naming its stairwell, index and
  prototype; and that the table holds no other rows. One document is held at
  a time, and each step is one primary-key lookup;
- checks every string ID, render layer index, ground count and portal target
  (plane and anchor, and a location within ±9,007,199,254,740,991) that a
  chunk references, that every palette entry is used, that ground cells and
  object prototypes name `proto` definitions and zone cells name zones of the
  chunk's plane, that field names match the field-name pattern and appear
  once per object, and that integer field values are within
  ±9,007,199,254,740,991, looking each target up once rather than loading
  the tables.

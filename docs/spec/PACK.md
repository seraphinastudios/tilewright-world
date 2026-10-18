# Tilewright content pack, version 1 (draft)

A content pack is the compiled, read-only form of a source tree
([FORMAT.md](FORMAT.md)). It is one SQLite database file named
`<sha256>.twpack`, where `<sha256>` is the lowercase hex SHA-256 of the file.
Game servers load it into memory at startup and may import it into
PostgreSQL; clients, bots and the browser (SQLite compiled to WebAssembly)
open it directly.

The compiler is deterministic: the same source tree and compiler version
always produce byte-identical packs. It builds the database in memory in one
transaction, inserting rows in primary-key order, runs `VACUUM`, and writes
the serialized database. The file uses `page_size = 4096`,
`application_id = 0x54575031` (`"TWP1"`) and `user_version = 1`.

## 1. Tables

The schema is exactly these seven tables, with no indexes, views or triggers
beyond SQLite's automatic ones:

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE strings (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
CREATE TABLE planes (id TEXT PRIMARY KEY, kind TEXT NOT NULL, chunk_size INTEGER NOT NULL, document TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE zones (plane TEXT NOT NULL, id TEXT NOT NULL, parent TEXT, document TEXT NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID;
CREATE TABLE anchors (plane TEXT NOT NULL, id TEXT NOT NULL, layer INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID;
CREATE TABLE chunks (plane TEXT NOT NULL, layer INTEGER NOT NULL, cx INTEGER NOT NULL, cy INTEGER NOT NULL, encoding TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (plane, layer, cx, cy)) WITHOUT ROWID;
CREATE TABLE definitions (type TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY (type, id)) WITHOUT ROWID;
```

`meta` holds exactly:

| Key | Value |
| --- | --- |
| `chunk_encoding` | `twc1+zstd` |
| `compiler` | The compiler's name and version, such as `tilewright-pack 0.1.0`. |
| `format` | `tilewright.pack/1` |
| `profile` | The profile document as canonical JSON. |
| `profile_id`, `profile_version` | From the profile. |
| `source_digest` | Lowercase hex SHA-256 over every source file read: for each path in byte order, the UTF-8 relative path, a zero byte, the file length as little-endian `u64`, and the SHA-256 of the file. |

`document` columns hold the source document (or, for `zones`, the zone's entry)
as canonical JSON: comments and whitespace removed, members in source order,
numbers spelled exactly as in the source, strings escaped only where JSON
requires.

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
length + canonical JSON) for arrays and objects. `custom` is canonical JSON
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
  hex `source_digest`;
- reject any chunk row whose `encoding` is not `twc1+zstd`, or whose `data` is
  not a blob within the bound of section 2, even when reading a single chunk;
- reject a string that is not text of at most 1 MiB, the source format's
  string limit, even when reading a single string;
- reject chunk records with an unknown version, flags or tags, out-of-range
  tiles, palette indexes or sizes, a palette that is not nonzero and strictly
  ascending, embedded JSON that is not canonical or not the required shape, a
  decompressed size above 16 MiB, or trailing bytes.

A full verification also:

- runs SQLite's integrity check, and compares the file's SHA-256 (hashed in
  bounded blocks) with its name when the name is a digest;
- checks that string IDs are exactly `1..=count` and every string is text of
  at most 1 MiB;
- checks that plane, zone, anchor and definition keys are identifiers, plane
  kinds and chunk sizes, zone planes and parents, and anchor planes and
  layers;
- checks that every `document` column, and the `profile` meta value, is
  canonical JSON of an object of at most 64 MiB whose identity agrees with
  its row: `format`, `id`, `kind` and `chunk_size` for planes, `id` and
  `parent` for zones, `format`, `type` and `id` for definitions, and `format`,
  `id` and `version` against `profile_id` and `profile_version` for the
  profile;
- checks every string ID, render layer index, ground count and portal target
  (plane and anchor) that a chunk references, looking targets up rather than
  loading the tables.

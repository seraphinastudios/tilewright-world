# Tilewright content pack, version 1 (draft)

A content pack is the compiled, read-only form of a source tree
([FORMAT.md](FORMAT.md)). It is one SQLite database file named
`<sha256>.twpack`, where `<sha256>` is the lowercase hex SHA-256 of the file.
Game servers load it into memory at startup and may import it into
PostgreSQL; clients, bots and the browser (SQLite compiled to WebAssembly)
open it directly.

The compiler is deterministic: the same source tree and compiler version
always produce byte-identical packs. It builds the database in memory in one
transaction, inserting rows in primary-key order, runs `VACUUM`, which leaves
no free pages, verifies the result as section 3 describes, and writes the
serialized database. The file uses `page_size = 4096`,
`application_id = 0x54575031` (`"TWP1"`) and `user_version = 2`. Version 2
added object uids: the `object_uids` table and version 2 chunk records.

## 1. Tables

The schema is exactly these ten tables, with no indexes, views or triggers
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
CREATE TABLE object_uids (uid TEXT PRIMARY KEY, plane TEXT NOT NULL, layer INTEGER NOT NULL, cx INTEGER NOT NULL, cy INTEGER NOT NULL, object INTEGER NOT NULL) WITHOUT ROWID;
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

`object_uids` holds one row per object that has a uid, at any depth: the
uid, the plane, layer and chunk coordinates of the object's chunk, and
`object`, the object's index in its chunk (FORMAT.md section 7), which is
its position in the chunk record's depth-first order (section 2). A reader
finds the object a uid names with one primary-key lookup and one chunk
decode, without scanning any table. The chunk records are authoritative;
`object_uids` indexes exactly their uids.

`strings` interns every string that chunk records reference: prototype IDs,
ground and zone palette values, field names, string field values, and portal
plane and anchor IDs. With terrain transitions (FORMAT.md section 13), it
also interns every prototype the transitions name, whether or not a chunk
draws it. IDs run from 1 in byte order of the values; 0 means "none"
wherever an ID is optional.

## 2. Chunk encoding `twc1+zstd`

`data` is a zstd frame (level 19, no dictionary, content size recorded)
containing a `twc1` record of at most 16 MiB. The frame itself is at most
16,842,752 bytes, zstd's compression bound for 16 MiB, so every encoded chunk
fits. All integers are little-endian.

```text
magic         4 bytes  "TWC1"
version       u16      2
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

Grids are row-major, `y` then `x`. Ground grids hold the chunk's tiles with
the profile's terrain transitions applied (FORMAT.md section 13): the
terrain layer's grid holds the painted terrain as the source does, and the
tiles the transitions derive are ordinary cells of the other ground grids,
so readers draw them without knowing the rules. An object record:

```text
tile          u16      y * N + x; 0xFFFF in inventories
proto         u32      string ID
layer         u16      render layer index + 1, 0 = prototype default
sub_layer     i16
z             i32
flags         u8       bit 0 portal, bit 1 fields, bit 2 custom, bit 3 inventory, bit 4 uid; other bits 0
[uid]         u8 length L (1..=128), then L bytes: the object's uid, an identifier
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
for an object. Objects appear in source order, each followed by its
inventory, so a record lists a chunk's objects in depth-first order;
inventories nest at most 16 deep. The optional parts of an object follow
its flags in the order above. Version 1 records had no uids; readers accept
only version 2.

## 3. Reading

Packs are untrusted input. Readers check a value's type and size in SQL before
loading it, and never load a whole table into memory. Readers:

- open the file read-only and immutable, with SQLite's defensive mode on and
  untrusted schemas disabled. A reader may instead open a pack from its
  bytes, as the browser does: it copies them into a read-only database in
  memory and checks them as it checks a file;
- reject the file unless its schema is exactly section 1, its
  `application_id` is `"TWP1"`, its `user_version` is 2, and `meta` holds
  exactly the keys of section 1, as text of at most 64 MiB for `profile` and
  1 KiB for the others, names this format and encoding, and has a lowercase
  hex `source_digest`, its pages are 4096 bytes, it has no free pages
  (`freelist_count = 0`), and the file is exactly its pages: its length is
  `page_count × 4096`, with nothing, not even a sparse hole, after the last
  page;
- reject any chunk row whose `encoding` is not `twc1+zstd`, or whose `data` is
  not a blob within the bound of section 2, even when reading a single chunk;
- reject a string that is not text of at most 1 MiB, the source format's
  string limit, even when reading a single string;
- reject a stairwell whose document is not canonical, does not have the
  source format's shape or disagrees with its row, even when reading a
  single stairwell, and a step row whose stairwell or prototype is not an
  identifier, even when reading a single step;
- reject an `object_uids` row whose plane is not an identifier, whose layer
  is not a 32-bit integer, whose chunk coordinates are not integers that
  keep every tile of the chunk within the source format's integer range, or
  whose `object` is not an integer from 0 to 16,777,215 (every object takes
  more than one byte of a record of at most 16 MiB), even when reading a
  single row; and, when reading the object a uid names, reject the row if
  the object at its index is not there or has another uid;
- check the whole stored profile, as a full verification does, before
  reporting its `layer_height`.

A single read checks the shape and bounds of what it loads. Whether the
targets it names exist (a step's stairwell or prototype, a chunk's
strings, the chunk a uid's row names) is checked by a full verification.
- reject chunk records with an unknown version, flags or tags, out-of-range
  tiles, palette indexes or sizes, a palette that is not nonzero and strictly
  ascending, a uid that is not an identifier, embedded JSON that is not
  canonical or not the required shape
  (custom data is an object whose keys are domain-qualified namespaces), data
  that is not exactly one zstd frame, a decompressed size above 16 MiB, or
  trailing bytes.

Readers also have a pack-size limit, which the caller sets and which is 8 GiB
by default. A file longer than the limit, or bytes more than it, are rejected
before SQLite opens or copies them, and again once open, by their
`page_count × 4096`. A full verification
checks the pages against it before its integrity check, and hashing the file
fails as soon as it has read more than the limit, even if the file has grown
since it was opened. Verification never uses a higher limit than the one the
pack was opened with. The `tilewright inspect` command sets the limit with
`--max-pack-bytes`.

Free pages hold no data, so a sparse file could keep any number of them as
holes that take no space on disk, which the integrity check skips but hashing
reads. A pack has none, and its integrity check confirms that every page
belongs to a table, an index or an overflow chain before the file is hashed,
so hashing reads only pages that the integrity check has already read.

Decoding is linear in a pack's decoded size, which compression can make
thousands of times its file size. A full verification therefore has a
decoded-byte budget, which the caller sets and which is 8 GiB by default, or
256 MiB in the browser reader, whose verification runs on the page's thread:
the sum of the decoded sizes the chunks' zstd frames record. It fails as soon as
the next chunk would pass the budget, before decompressing that chunk. The
`tilewright inspect` command sets the budget with `--max-decoded-bytes`.
A reader that builds a value for each object and field of a chunk, or step
of a stairwell, as the browser reader does, may also give single reads a
budget of them, counted before any is built.
Verification checks each chunk's objects as they decode instead of building
the chunk's object model, and keeps its target lookups in caches of bounded
size, so its memory stays bounded by one record.

A full verification also:

- runs SQLite's integrity check, and then compares the file's SHA-256
  (hashed in bounded blocks, within the pack-size limit) with its name when
  the name is 64 hex digits, which must be the lowercase digest. The digest
  is computed from the file at the pack's path when the check runs, so it
  binds to the path, not to the file the database connection opened; packs
  are content-addressed files opened from directories the caller controls.
  A pack opened from bytes has no name: its caller may give the expected
  digest instead, which verification compares with the SHA-256 of the copy
  the database reads;
- checks that string IDs are exactly `1..=count` in byte order of their
  values, and every string is UTF-8 text of at most 1 MiB without NUL;
- checks that plane, zone, anchor, stairwell and definition keys are
  identifiers; plane kinds, and a chunk size of 32 as format 1 requires; zone
  planes, parents and that parent chains end at a root, following one
  plane's chains at a time after checking that the plane has at most
  250,000 zones, the array limit of the one source document that holds
  them; anchor planes, layers
  and positions; stairwell planes; that every
  `stairwell_steps` row names a stairwell of its plane and a `proto`
  definition, with a 32-bit layer, a position within ±9,007,199,254,740,991
  and an index from 0 to 249,999, as in a source array; and chunk planes, layers and coordinates, which
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
  tile per axis, and a valid direction and flag; that a plane has at most
  250,000 stairwells and 1,000,000 steps, the array and value limits of the
  one source document that holds them; that each step
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
  the tables;
- checks that every `object_uids` row has a uid that is an identifier,
  integer layer and chunk coordinates that name a chunk, and an `object`
  from 0 to 16,777,215; that each object with a uid, at any depth, is the
  one its uid's row names, the row's chunk and index being the object's;
  and, after the chunks, that the table has as many rows as there are
  objects with uids. Each such object costs one primary-key lookup. Two
  objects with one uid would need its one row to name both, so this also
  checks that uids are unique in the pack.

## 4. Error codes

Readers report why they reject a pack with one of these stable codes. The
conformance fixtures in `fixtures/packs` pin the code each broken pack
fails with, and every reader in this repository runs them.

| Code | Meaning |
| --- | --- |
| `pack.io` | The file could not be read. |
| `pack.sqlite` | SQLite failed, for example on a file that is not a database. |
| `pack.not_a_file` | The path is not a regular file. |
| `pack.too_large` | The file is longer than the pack-size limit. |
| `pack.file_length` | The file is not exactly its pages. |
| `pack.schema` | The schema is not exactly section 1. |
| `pack.identity` | The `application_id`, `user_version` or page size is not the format's. |
| `pack.free_pages` | The pack has free pages. |
| `pack.meta` | `meta` does not hold exactly the keys and values of section 1. |
| `pack.profile` | The stored profile is invalid or disagrees with `meta`. |
| `pack.string` | A string is not UTF-8 text of at most 1 MiB without NUL. |
| `pack.string_ids` | String IDs are not `1..=count` in byte order of their values. |
| `pack.plane` | A plane row or document is invalid. |
| `pack.zone` | A zone row or document is invalid, or zone parents form a cycle. |
| `pack.anchor` | An anchor row is invalid. |
| `pack.stairwell` | A stairwell row or document is invalid or disagrees with its steps. |
| `pack.stairwell_step` | A `stairwell_steps` row is invalid or listed by no stairwell. |
| `pack.definition` | A definition row or document is invalid. |
| `pack.chunk` | A chunk row's key is invalid, or its record disagrees with its plane or the profile. |
| `pack.chunk_encoding` | A chunk's `encoding` is not `twc1+zstd`. |
| `pack.chunk_data` | A chunk's `data` is not a blob within the bound of section 2. |
| `pack.chunk_record` | A chunk's data is not a valid `twc1+zstd` record. |
| `pack.reference` | A chunk record names a target that does not exist or is out of range. |
| `pack.object_uid` | An `object_uids` row is invalid, or the table does not index exactly the uids of the chunk records. |
| `pack.integrity` | SQLite's integrity check failed. |
| `pack.digest` | The file's SHA-256 does not match its name, or a pack opened from bytes does not have the digest its caller expects. |
| `pack.decode_budget` | The chunks decode to more than the verification budget, or a single read past the reader's budget of objects, fields or steps. |

Text that is not UTF-8 in a value a reader loads gets the code of that
value's rule, not `pack.sqlite`. A pack that breaks several rules may be
reported with the code of any of them; each fixture breaks one.

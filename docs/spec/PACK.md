# Tilewright content pack, version 1 (draft)

A content pack is the compiled, read-only form of a source tree
([FORMAT.md](FORMAT.md)). It is one SQLite database file named by the SHA-256
digest of its contents. Game servers load it into memory at startup and may
import it into PostgreSQL; clients, bots and the browser (SQLite compiled to
WebAssembly) open it directly. The compiler is deterministic: the same source
tree and compiler version always produce byte-identical packs.

## 1. Tables

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE strings (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
CREATE TABLE planes (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, chunk_size INTEGER NOT NULL,
  document TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE zones (
  plane TEXT NOT NULL, id TEXT NOT NULL, parent TEXT, document TEXT NOT NULL,
  PRIMARY KEY (plane, id)
) WITHOUT ROWID;
CREATE TABLE anchors (
  plane TEXT NOT NULL, id TEXT NOT NULL, layer INTEGER NOT NULL,
  x INTEGER NOT NULL, y INTEGER NOT NULL, PRIMARY KEY (plane, id)
) WITHOUT ROWID;
CREATE TABLE chunks (
  plane TEXT NOT NULL, layer INTEGER NOT NULL, cx INTEGER NOT NULL,
  cy INTEGER NOT NULL, encoding TEXT NOT NULL, data BLOB NOT NULL,
  PRIMARY KEY (plane, layer, cx, cy)
) WITHOUT ROWID;
CREATE TABLE definitions (
  type TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL,
  PRIMARY KEY (type, id)
) WITHOUT ROWID;
```

`meta` holds at least `format` (`tilewright.pack/1`), `profile_id`,
`profile_version`, `source_digest` (SHA-256 over the sorted source file paths
and bytes) and `compiler`. `document` columns hold canonical minified JSON
with comments removed and keys in source order. `strings` interns every
prototype ID, field name and zone ID that chunk blobs reference.

Rows are inserted in primary-key order and the file is written with
`page_size = 4096`, `journal_mode = OFF`, then `VACUUM`ed, so identical input
yields identical bytes.

## 2. Chunk encoding `twc1+zstd`

`data` is a zstd frame (level 19, no dictionary, content size recorded)
containing a `twc1` record. All integers are little-endian.

```text
magic        4 bytes  "TWC1"
version      u16      1
chunk_size   u16      N
palette_len  u32      P
palette      P × u32  string IDs (index 0 = first entry)
floor        N² × u16 palette index + 1, 0 = empty   (row-major, y then x)
mask         N² × u16 palette index + 1, 0 = empty
height       N² × i32
zone         N² × u16 palette index + 1, 0 = plane default zone
object_count u32
objects      object_count × object
```

An object record:

```text
tile         u16      y * N + x (absent in inventories: 0xFFFF)
proto        u32      string ID
layer        u16      render layer index + 1, 0 = prototype default
sub_layer    i16
z            i32
flags        u8       bit 0 portal, bit 1 inventory, bit 2 fields
[portal]     u8 kind (0 location, 1 anchor), then
             location: u32 plane string ID (0 = same plane), i32 layer, i64 x, i64 y
             anchor:   u32 anchor string ID
[fields]     u16 count, then count × (u32 name string ID, value)
[inventory]  u32 count, then count × object
```

A field value is a tag byte and payload: `0` null, `1` false, `2` true,
`3` i64, `4` f64, `5` string (u32 string ID), `6` JSON (u32 byte length +
canonical minified JSON) for arrays and objects.

Objects appear in source order. Readers reject unknown versions, out-of-range
indexes, trailing bytes and decompressed sizes above 16 MiB.

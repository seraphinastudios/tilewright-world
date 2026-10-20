# Tilewright ID diff, version 1 (draft)

Status: draft, like [FORMAT.md](FORMAT.md) and [PACK.md](PACK.md).

Games keep IDs in saves, server state and scripts: the planes, zones,
anchors, stairwells, definitions and render layers of a world. Removing or
renaming one breaks whatever refers to it. `tilewright diff OLD NEW`
compares the IDs of two worlds and reports which were removed, added and
renamed, so that content CI can compare a change with a baseline, such as
the last released pack, and fail on a removal or rename nobody intended.

```sh
tilewright diff [--json] [--allow ALLOWLIST] [--max-ids COUNT] OLD NEW
```

## 1. Inputs

Each side is a source root ([FORMAT.md](FORMAT.md)) or a content pack
([PACK.md](PACK.md)): a path that is a file is read as a pack, and any other
path as a source root. Both sides are untrusted and read with the default
bounds. A source root is loaded and validated by the reference loader. A
pack is opened and fully verified, as `tilewright inspect` does, before any
of its IDs is read. A side that does not load is a load error (section 7).

A comparison keeps every ID of both sides in memory, so each side may hold
at most `--max-ids` IDs of all kinds, 1,000,000 by default. Reading stops at
the first ID past the limit, before it is kept, and the side is a load
error.

A reference that no longer resolves, such as a chunk that still names a
removed prototype, makes the new world invalid: the loader reports it as
`ref.unresolved`, and pack verification rejects it. `diff` reports it as a
load error, with the loader's diagnostics, rather than as an ID change.

Until format 1.0 is frozen, a pack from an older compiler may not open with
a newer one. Compare source trees instead, such as a checkout of the
baseline revision.

## 2. ID kinds

| Kind | Written | Unique within | Content compared for renames |
| --- | --- | --- | --- |
| `anchor` | `<plane>/<id>` | its plane | its `layer` and `at` |
| `definition` | `<type>/<id>` | its type | its document without `id` |
| `plane` | `<id>` | the world | its document without `id` and `title`; the IDs and content of its zones, anchors and stairwells; and the addresses (layer and chunk coordinates) of its chunks, not their contents |
| `render_layer` | `<name>` | the profile | its position in `render_layers` |
| `stairwell` | `<plane>/<id>` | its plane | its entry without `id` |
| `zone` | `<plane>/<id>` | its plane | its entry without `id` and `name` |

Every part of a written ID is an identifier (FORMAT.md section 2), so `/`
never appears inside one. Documents and entries are compared as canonical
JSON (PACK.md section 1): comments, whitespace and line breaks do not
matter, but member order and the spelling of numbers do. Display names
(`title`, `name`) are left out because they often change with the ID.

These are not kinds of their own:

- definition types: removing a type removes its definitions, which are
  reported;
- ground layers, which are render layers;
- the profile's `id` and `version`, which the JSON output shows for each
  side but which are not compared;
- chunks, which are addressed by position, not by ID.

## 3. Changes

An ID of a kind is *removed* when the old world has it and the new one does
not, and *added* when the new world has it and the old one does not.

A *rename* pairs one removed ID with an ID of the new world of the same kind.
Pairs are found in this order:

1. **Declared.** An allowlist `rename` entry (section 4) pairs its `from`
   with its `to` when `from` was removed and `to` is an ID of the new world.
   `to` need not be new: renaming into an existing ID merges two IDs.
2. **Same content.** Of the removed and added IDs left, a removed ID and an
   added ID pair when they are the only removed and the only added ID with
   the same kind, scope and content (section 2). The scope is the plane for
   zones, anchors and stairwells, the type for definitions, and the world
   for planes and render layers. The zones, anchors and stairwells of a
   renamed plane count as being in its new plane, so planes are paired
   first.

There is no other matching: IDs are never paired by similar names or
similar content. A same-content pair is a likely rename, not a certain
one. An ID whose content changed in the same change as its ID is reported as
removed and added, unless a `rename` entry declares it. So is a renamed ID
whose content refers to another renamed ID, such as a definition that names
a renamed prototype, since its content changed too.

The removal of an ID, including the old ID of a rename, is *breaking*
unless the allowlist accepts it. Additions are never breaking. A change to an
ID's content that keeps the ID is not reported.

## 4. Allowlist

`--allow` names a strict JSONC document (FORMAT.md section 3) that declares
the removals and renames a change intends:

```jsonc
{
  "format": "tilewright.diff-allow/1",
  // Retired with the winter event.
  "remove": [
    {"kind": "definition", "id": "proto/snowman"}
  ],
  "rename": [
    {"kind": "zone", "from": "overworld/village", "to": "overworld/hamlet"}
  ]
}
```

`format` is required; `remove` and `rename` are optional arrays. A `remove`
entry has exactly `kind` and `id`, and a `rename` entry exactly `kind`,
`from` and `to`. `kind` is a kind of section 2, IDs are written as the kind
requires, a rename's `from` and `to` differ, and each old ID (`id` or
`from`) has at most one entry of either list. Any other key, value or
duplicate is an error, reported with its line, column and a stable code
(`jsonc.*`, `shape.*`, `ref.duplicate`, `source.io` or `source.encoding`),
and `diff` exits 2.

A removed ID, or the old ID of a rename, is *allowed* when:

- a `remove` entry names it;
- a `rename` entry names it, and its `to` is an ID of the new world; or
- it is a zone, anchor or stairwell of a plane that the new world lacks,
  and the plane's entry removes the plane, or renames it to a plane of the
  new world that has an ID of the same kind with the same local ID.

An entry that does not apply is reported as *unused*: `not_removed` when its
ID is not in the old world or is still in the new one, and otherwise
`target_missing` when a rename's `to` is not in the new world, in which case
the removal is not allowed. An unused entry does not change the exit code on
its own.

## 5. Text output

By default, `diff` prints one line per removed ID, then per rename, then per
added ID, then per unused entry, each group sorted by kind and ID, and a
summary:

```text
removed definition proto/snowman (allowed)
renamed anchor overworld/village.well -> overworld/village.fountain (same content)
renamed zone overworld/village -> overworld/hamlet (declared, allowed)
added definition proto/oak
unused allowance: remove plane vault (not removed)
summary: 1 removed, 2 renamed, 1 added, 1 not allowed
```

A rename says `same content` or `declared`; `allowed` marks an accepted
removal. IDs are identifiers, so no line carries untrusted text.

## 6. JSON output

`--json` prints the report as one line of JSON, an object with these members
in this order:

| Member | Value |
| --- | --- |
| `format` | `"tilewright.diff/1"` |
| `old`, `new` | Each side: `{"input", "profile", "profile_version", "source_digest"}`, where `input` is `"source"` or `"pack"`, the profile values are the profile's `id` and `version`, and `source_digest` is the lowercase hex digest of PACK.md section 1, which a source tree and its pack share. |
| `breaking` | `true` when a removed or renamed entry has `"allowed": false`. |
| `removed` | `[{"kind", "id", "allowed"}]`, sorted by kind, then ID. |
| `renamed` | `[{"kind", "from", "to", "basis", "allowed"}]`, sorted by kind, `from`, then `to`; `basis` is `"declared"` or `"content"`. |
| `added` | `[{"kind", "id"}]`, sorted by kind, then ID. |
| `unused` | `[{"allowance", "kind", "id", "reason"}]` for a removal and `[{"allowance", "kind", "from", "to", "reason"}]` for a rename, sorted by kind, then old ID; `allowance` is `"remove"` or `"rename"` and `reason` is `"not_removed"` or `"target_missing"`. |

Sorting is by byte order, and kinds sort by name. Every control character
in a string is escaped as `\u00XX`. The same two worlds and allowlist always
give byte-identical output; the input paths are not part of it. A later
version that changes the meaning of a member gets a new `format`.

## 7. Exit codes

| Code | Meaning |
| --- | --- |
| 0 | No ID was removed or renamed, or the allowlist accepts every one. |
| 1 | An ID was removed or renamed and the allowlist does not accept it. |
| 2 | A side, the allowlist or the arguments could not be read. |

With code 2, nothing is printed on standard output. Standard error says
why: a source side's diagnostics, one per line prefixed with `old: ` or
`new: `, a pack side's first problem, a side past the ID limit, the
allowlist's first problem, or the usage.

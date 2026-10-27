# Tilewright transactions, version 1 (draft)

A transaction is a reviewable edit of a source tree ([FORMAT.md](FORMAT.md)).
Editors, the command line and agents all express a change as one: a human
click and an agent call produce the same document. It is a log of operations
against a base revision, not a byte patch:

- operations address content by identity (a tile, an object's place on its
  tile or its uid, a document path and a pointer), never by byte offset;
- each operation carries every value it writes, so replaying a transaction on
  the same files gives the same bytes;
- when the base moves on, the transaction is rebased by replaying its
  operations on the new files, and a conflict is a failed precondition on a
  specific cell, object or value;
- previewing writes nothing, and applying checks every precondition again,
  publishes the files and yields an exact inverse, which undo applies.

Chunks are not named by operations on tiles. An edit that spans chunks is one
transaction, and chunk files are created and removed as content appears and
disappears.

## 1. Document

A transaction is a strict JSONC document (FORMAT.md section 3):

```jsonc
{
  "format": "tilewright.transaction/1",
  "title": "Plant the village orchard",          // optional, at most 256 characters
  "actor": "agent:orchard-planner",              // optional, at most 256 characters
  "base": [                                      // optional
    {"path": "planes/overworld/layers/0/0_0.jsonc", "sha256": "6d878f09d3eeec75575831cb2f284693750bd05b30c3935be943038a204908ca"},
    {"path": "planes/overworld/layers/0/1_0.jsonc", "sha256": null}
  ],
  "result": [ ... ],                             // optional, the same shape
  "operations": [
    {"op": "object.insert", "plane": "overworld", "layer": 0, "at": [40, 6], "object": {"proto": "tree"}},
    {"op": "object.move", "uid": "village.chest", "expect": {"proto": "chest"}, "to": {"at": [41, 6]}},
    {"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "floor",
     "cells": [{"at": [5, 5], "value": "sand", "expect": "grass"}]}
  ],
  "custom": {"example.org/editor": {"command": "brush", "seed": 1234}}
}
```

| Key | Meaning |
| --- | --- |
| `format` | `tilewright.transaction/1`. |
| `title`, `actor` | What the change is and who or what made it, for review and provenance. |
| `base` | The files the transaction touches or consults: one `{"path", "sha256"}` entry each, with the lowercase hex SHA-256 of the file's bytes, or `null` for a file that must be absent. A path appears once. |
| `result` | The same files after the transaction. |
| `operations` | The operations, carried out in order. |
| `custom` | Namespaced extension data, as in FORMAT.md section 7, such as the editor command, its parameters and its seed. |

`base` and `result` are usually written by the toolkit: carrying a
transaction out records every file it read, created or checked for absence
(section 7). A transaction without them applies to the tree as it is.

## 2. Addressing

- **Documents** are named by root-relative path: `world.jsonc`, or a `.jsonc`
  file under `profile/`, `planes/` or `defs/`. Components are ASCII letters,
  digits, `_`, `.` and `-`, not starting with `.`; a path has at most 1,024
  bytes, 32 components and 128 bytes a component. No other file can be
  touched.
- **Tiles** are `"plane"`, `"layer"` and `"at": [x, y]` in plane coordinates.
  The tile's chunk is `planes/<plane>/layers/<layer>/<cx>_<cy>.jsonc` with
  `cx = floor(x / 32)`, and its chunk-local position is `x - 32 cx`;
  likewise for `y`. Positions are limited to the chunks a plane can hold
  (FORMAT.md section 2).
- **Objects** are named in one of two ways. A tile and an `"index"` name
  the object at that place among the objects on the tile, in list order,
  which is their stacking order; edits elsewhere in the chunk do not move
  it. A `"uid"` alone names the object with that uid (FORMAT.md section 7)
  wherever it is, on any tile, chunk, layer or plane or in an inventory
  (section 2.1). An object address that mixes the two, or has any other
  key, is an error. Operations that remove or move an object name what
  they expect it to be, so that an object that moved or changed is a
  conflict, not an edit of another object. An object keeps every member as
  written, its `uid` included, when it is moved, and an inverse restores it
  byte for byte; a transaction that would leave two objects with one `uid`
  fails validation with the loader's `ref.duplicate`.
- **Values** are a document and a `"pointer"`: an array of steps, each a key
  (a string), an array index (an integer from 0 to 250,000) or
  `{"id": "<identifier>"}`, the one array item that is an object whose `id`
  is that identifier. `["zones", {"id": "island"}, "name"]` names a zone's
  name whatever its position. An object target, `"object": {"plane",
  "layer", "at", "index"}` or `"object": {"uid"}`, takes the place of
  `"path"` and makes the pointer relative to the object. That pointer names
  a part of the object: it is not empty and does not start with `at`, which
  is the object's tile, and an `insert` inserts into a part of the object,
  so its pointer has a step before the index. A `set` adds a member to the
  object itself.

Values in operations are written exactly as they appear in the transaction,
comments and spacing included, and keys exactly as spelled there.

### 2.1 Uid addresses

A uid address is resolved when its operation is carried out, in the world
as loaded before the transaction and as the operations before it left it.
So it names its object after an insertion before it on its tile, a
reorder, or a move to another tile, chunk, layer or plane, whether an
earlier operation made the change or the base did and the transaction is
rebased (section 7).

- The tree is loaded once, before the operations. A tree that does not
  load resolves no uid: every operation with a uid address is then a
  `tx.conflict` naming the loader's first problem.
- A uid that no object has, such as one an earlier operation removed or
  changed, or that more than one object has, is a `tx.conflict`, as is an
  object that is not what the operation's `expect` says.
- Resolving consults the chunk that holds the object, which `base` records
  as it does a chunk a tile names. Uids are unique in a valid world and the
  result is validated, so that chunk names the object as surely as a tile
  does.
- An inventory entry's uid makes a value pointer relative to the entry, and
  `object.remove` takes the entry out of its inventory. `object.move` moves
  objects that are on tiles: an entry is moved by removing it from its
  inventory and inserting it where it goes, and moving it is a
  `tx.conflict`.
- An object a transaction creates has the uid the transaction writes, in an
  `object.insert` object, an inserted value or a `set` of its `uid`; later
  operations can address it by that uid. The toolkit never assigns a uid,
  since a transaction carries every value it writes: a tool that writes the
  transaction chooses it, as FORMAT.md section 7 recommends.

The uid form was reserved when this version was drafted. A transaction
without one means what it did, and a reader that predates it rejects one
with it as `tx.shape` rather than misreading it, so it does not change the
`format`.

## 3. Operations

Every operation is an object with `"op"` and the keys below; any other key is
an error.

| Operation | Keys | Effect |
| --- | --- | --- |
| `set` | target, `pointer`, `value`, `expect`? | Replaces the value at `pointer`, or adds a missing member at the end of its object. |
| `insert` | target, `pointer`, `key`?, `value`, `before`?, `after`?, `colon`? | Inserts an item into the array or object named by `pointer` without its last step, at the index that last step gives. An object member needs `key`. |
| `remove` | target, `pointer`, `expect`?, `separator`? | Removes an array item or object member. |
| `test` | target, `pointer`, `value` or `"absent": true` | Checks a value, or that there is none; an absent test may end in `{"id": ...}` to check that no entry has that identity. |
| `object.insert` | tile, `index`?, `object` | Inserts `object` (without `at`) at `index` among the tile's objects; by default after them. |
| `object.remove` | object, `expect` | Removes an object. |
| `object.move` | object, `expect`, `to` | Moves an object to `to`: `{"plane"?, "layer"?, "at"?, "index"?}`, each defaulting to the object's own (and `index` to after the destination's objects). `to.index` counts the destination's objects without the moved one, so a move on one tile reorders it, and a move to where the object already is changes nothing. |
| `terrain.set` | `plane`, `layer`, `grid`, `cells` | Sets terrain cells of one grid (section 5). |
| `document.create` | `path`, `document` or `text` | Creates a document: `document` is written in canonical layout (section 10), `text` exactly; `text` is a string or an array of strings joined, for a file longer than a string or a line may be. Either must be valid JSONC. |
| `document.delete` | `path` | Deletes a document. |
| `document.rename` | `path`, `to` | Moves a document to a path that is free, unchanged. A rename that changes identity also sets it, such as a definition's `id` or a chunk's `chunk`, in the same transaction; references to it are updated by operations of their own. |

Target is `"path"` or `"object"` (section 2). Object is an object address
(section 2) given in the operation itself: `"plane"`, `"layer"`, `"at"` and
`"index"`, or `"uid"`. An object operation's `expect`
is an object that names at least `proto` and not `at`; each member it names
must equal the object's. A `set` or `remove` with `expect` requires the
current value to equal it; equality is equality of canonical JSON (PACK.md
section 1), so member order and number spelling count.

### 3.1 Separators

An array's items, or an object's members, sit between regions of whitespace
and comments, and the regions between two items hold the comma:
`[R0 item0 R1 item1 ... Rn]`. Edits keep every byte outside the regions they
change:

- `insert` at index `i` replaces region `Ri` with `before`, the item and
  `after`. Given, `before` holds the comma unless the item is first and
  `after` holds it unless the item is last; both are otherwise whitespace and
  complete comments. A member's key and value are joined
  by `colon` (whitespace and comments around one `:`), which only an
  insertion with `key` may give. Like `text`, each of these, and a
  removal's `separator`, is a string or an array of strings joined.
- Without them, the new item takes its neighbours' line style: in a
  container that puts items on their own lines it gets its own line at their
  indentation, in one that does not it follows a comma and a space, and
  comments stay where they are. A member's colon is the first member's if
  that is plain, or `": "`.
- `remove` replaces the item and its two regions with `separator`, which holds
  a comma only when the item had neighbours on both sides. Without it, the
  comments between the item and the commas that separate it from its
  neighbours go with it, other comments stay, and the item's line goes when
  items are on their own lines.

Inverses (section 8) record the exact regions, so undoing an edit restores
every byte.

## 4. Chunks

An object or terrain edit on a tile whose chunk file is missing creates the
file with its header (`format`, `plane`, `layer`, `chunk`) in canonical
layout, then edits it; an `objects` array or `terrain` object that a chunk
lacks is added in the same way. After the operations, every chunk that tile
operations touched and that holds no content (no objects, and every terrain
grid empty: no cell set, every height 0) is deleted. Applying a transaction
creates the directories new files need and removes those that deleting files
left empty, below `planes/`, `defs/` and `profile/`, unless one's mode or
group is not that of the directory above it (section 9).

## 5. Terrain

`terrain.set` takes `grid`, a ground layer name, `height` or `zone`, and
`cells`: `{"at": [x, y], "value", "expect"?}`, each tile at most once. A
ground or zone value is an identifier or `null` for an empty cell; a height
is a 32-bit integer. A cell's `expect` is its current value; a missing grid
or chunk reads as empty, and heights as 0. Cells may lie in any number of
chunks. In each chunk the grid is edited in place:

- a row that changes is replaced; in a height grid each changed height is;
- a value that the palette lacks gets a new entry at its end, keyed by the
  value's first characters if free, or else by the first free key of
  `a`-`z`, `A`-`Z`, `0`-`9` and then the other visible ASCII characters
  except `.`, `"` and `\`; an entry that the edit left unused is removed,
  while unused entries the edit did not touch stay;
- a `fill` grid whose cells are all one value stays a `fill`; otherwise its
  `fill` member becomes `rows`;
- a grid whose cells all end up empty, or all heights 0, is removed;
- a new grid is added in canonical layout, ground layers before `height` and
  `height` before `zone`;
- a grid whose palette has no free key of its `cell_width` is rewritten with
  `cell_width` 2 and only the entries it uses.

The operation must name a plane of the edited world and a ground layer of
its profile, `height` or `zone`, even when it leaves no content to write.
Once the edited tree validates, an address that does not is a
`tx.conflict` at the operation. Cells written at such an address make the
tree fail validation first, and the loader's diagnostics at the chunk say
so.

## 6. Carrying out

Carrying out a transaction is pure and deterministic. It reads each file it
needs once, through the loader's path confinement: no symbolic link beneath
the root is followed, and on Unix every component is opened through its
parent's handle. Every edit is a span edit of the file's JSONC, which is
parsed again after each edit, so untouched bytes, comments and ordering stay.
The edited tree is then validated by the normal loader, with the edited files
in memory over the files on disk. Previewing stops there and writes nothing;
it returns the edited files, the validated world, the sealed transaction and
the inverse. A transaction whose result does not validate is rejected with
the loader's diagnostics.

The sealed transaction is the transaction's own text, with `base` and
`result` set to every file it touched or consulted, before and after; every
other byte, comments included, is kept.

## 7. Preconditions, conflicts and rebasing

Applying checks, in this order:

1. `base`, when present: every file it lists must have that digest, or be
   absent for `null`;
2. every operation's preconditions: `expect` and `test` values, cell
   expectations, and what an operation needs, such as an object at an index,
   a member to remove, an absent path to create or a present one to delete;
3. that every file the operations touched or consulted is in `base`, when
   present;
4. that the result has the digests of `result`, when present: replaying
   gives the reviewed bytes or nothing.

A failed check is a `tx.conflict` diagnostic at the operation, or at the
cell, it concerns, with a message that names the file and the value, such as
`planes/overworld/layers/0/0_0.jsonc: floor cell (0, 0) is "grass", expected
"sand"`. A failing operation changes nothing, and the operations after it
are still carried out on the files without it, so every failing operation
is reported, not only the first.

Rebasing replays the operations on the tree as it is, ignoring `base` and
`result`, with every other check. Uid addresses are resolved again, so an
operation follows its object wherever the base moved it. When it succeeds,
the sealed transaction records the new base. A rebase that fails reports the conflicts; the change
that caused them is in the files they name.

## 8. Inverse

Carrying out a transaction also gives its inverse, a transaction that,
applied after it, restores every file byte for byte. It holds, for each file
the transaction changed, in path order:

- `document.delete` for a file it created;
- `document.create` with the exact `text` for a file it deleted, in pieces
  of at most 128 KiB, one a line;
- otherwise the exact inverse of each edit, last first: `set` with the old
  value's text, `insert` with the old item, key, colon and regions, `remove`
  with the old region. An old value or key, and each step of a pointer, is
  written on a line of its own, so no line of the inverse is longer than a
  line of the file.

An inverse names files by path and values by pointer, never objects by
place or uid, whatever addresses the transaction used: it restores bytes,
so each of its operations depends on its own file alone.

An inverse holds text, not permissions: a file it creates again takes the
permissions of the directory it is in, which were the file's own as far
as reading goes (section 9).

Its `base` is the changed files after the transaction, and its `result` the
files before. Every operation of an inverse names one file by path and
checks nothing first, so its files are independent: when a transaction of
that kind, with no `test` and no `expect`, has a `result`, a file that
already has its result digest is left alone instead of being a conflict. Applying an inverse therefore undoes an interrupted apply, and
applying it twice does nothing the second time. An inverse is applied like
any transaction, so its result must validate: the inverse of a transaction
that made an invalid tree valid recreates the invalid tree, is refused, and
such a tree is restored from version control instead.

## 9. Applying

Publishing is supported on Linux, macOS and Windows, with these
guarantees:

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Reaches files | Through directory handles | Through directory handles | By path, each component checked first |
| A link swapped in by another process during the apply | Never followed | Never followed | Can redirect a write, rename or removal |
| Access control lists | Refused on the local file systems below; on any other neither checked nor kept | Neither checked nor kept | Neither checked nor kept |
| Labels (SELinux, Smack) | Compared, never set | None | None |
| The lock | `flock` of the root | `flock` of the root | The file `.tilewright.lock` in the root |
| Directory syncs | Yes | Yes, with `F_FULLFSYNC` where the file system has it | No: renames may not be durable yet |
| Publishes by default | On the local file systems below; on any other only with unchecked access | No: only with unchecked access | No: only with unchecked access |

Only on Linux does publishing keep every guarantee of this section, and
there only on the local file systems that keep their access control lists
as the extended attributes it reads (below): ext2, ext3, ext4, XFS, Btrfs,
tmpfs, F2FS, ZFS, overlayfs and bcachefs. On macOS and Windows it is a
best effort: it is supported only in trees that no one else can change,
and in which no file or directory has an access control list of its own
(on Windows, a file has the list its directory gives it), since a
replaced file, or one an inverse makes again, takes the access its
directory gives a new file, and nothing can check that from safe code.
The same holds on Linux for a tree, or a directory above a file the
transaction changes, on any other file system, which publishing tells by
its type (`statfs`): network, host-shared (WSL's mounts of Windows drives,
a virtual machine's shared folders), FUSE and NTFS ones among them, whose
lists Linux may not show at all. So in each of these cases publishing is
refused by default, with `tx.io`, and nothing is written: it goes ahead
only when the caller accepts what is not checked, with unchecked access
(the library's `PublishOptions::unchecked_access`, the command line's
`--unchecked-access`), and the command line then warns, naming it, and on
Linux the file system, by name or by its magic number. Elsewhere
unchecked access changes nothing. On other systems, the BSDs included,
publishing is refused, with `tx.io`, and nothing is written: they are
untested. Previewing and rebasing work everywhere.

Publishing a carried-out transaction works on the root it was carried out
on: through directory handles, the directory opened then, whatever its
path names later; on Windows, the absolute path it was opened at. It:

1. takes the source root's exclusive lock (an advisory `flock` of the root
   directory; on Windows the file `.tilewright.lock` in the root, created
   afresh, shared with no one and deleted when released, even by a process
   that stops, so a file by that name that no apply holds is an error), so
   two applies cannot interleave, and reads
   the permissions of the files it changes and of the directories above
   them (below);
2. writes each new file beside its target as `.<name>.tilewright-tmp`,
   created fresh (whatever was at that name is removed first, so a link or
   hard link there is never written through); checks, before a byte is
   written, the labels the system gave it (on Linux, below); writes it,
   only its owner's on Unix until then; gives it the mode and group given
   below, without an access control list its directory would give it on
   Linux; and syncs it. Beside each file it deletes it stages an empty file the same
   way, with the access a file made there again gets, as the inverse would
   make it, so that a file is deleted only if its inverse can make it again
   (below); that file goes with the deleted one. A failure removes every
   staging file it wrote and the directories it created, and reports one it
   created that is no longer empty;
3. reads every file the transaction touched or consulted again and compares
   it with the files it was carried out on, then validates the tree with
   the transaction's files again, and its terrain addresses against that
   tree, since files it did not touch may have changed since it was carried
   out: two transactions that are valid alone can be invalid together. A
   difference is a `tx.conflict`, and a result that no longer validates is
   reported with the loader's diagnostics. Last, the permissions and owners
   that step 1 read must still be those on disk. A file grown past what can
   be read is a difference too. On any of these the staged files are
   removed and nothing is written;
4. renames each staged file over its target, in path order, and then
   removes each deleted file, in path order, so that a renamed file is
   never only removed. Before a deleted file is removed, any staging file
   beside it goes, so that a failure there leaves the file;
5. removes the staging files left beside any file the transaction touched or
   consulted, removes the directories deleting files emptied, and syncs
   every directory whose entries changed. An emptied directory whose mode
   or group is not that of the directory above it is left, empty: an
   inverse makes a directory again with the access of the nearest one
   that is there, so it would make it, and the file in it, otherwise. A staging entry or an emptied
   directory that cannot be removed is reported, the former since the tree
   does not validate while it is there.

A transaction moves content between files: a renamed file's, a moved
object's, a deleted file's that its inverse creates again. So the files it
changes and every directory above them, up to the root, must be reachable
by the same users, which publishing checks before it writes anything and
again just before it renames: the classes that may read each file, and
search each directory, must be the same, where the group may and others
may not, the group must be the same, and where not everyone may, the
owner must be the same. Otherwise publishing is
refused with `tx.io`, naming two that differ, and nothing is written. So
is a mode whose classes do not nest, one that lets others read, or
search, what it denies the group, or the group what it denies the owner
(such as `0604`): who may read under it depends on who owns the entry and
which group it has, and a file publishing writes is owned by the user
applying. A
file written where a file is keeps that file's mode and group. A new file
takes the mode of the directory above it without the execute bits, and its
group, and a new directory the mode and group of the directory above it;
every file staged, the empty one beside a deleted file and a new file
included, every file restored and every directory made must then be
reachable by the same users as the files and directories compared, its
owner, the user applying, counting where not everyone may read: so another
user's restricted file is not replaced, a file is deleted only if its
inverse makes it again for the users who could read it, and a file is made
only if a later transaction, its inverse among them, could change it. A group the user may not give is an
error, since the group bits would otherwise apply to another group. The
owner of every file written is the user applying, who could read them all.
Where there are no modes (Windows), only the read-only attribute is
compared, and a read-only file is refused, as is an encrypted (EFS) file
or directory, since a file written in its place would not be encrypted.

On Linux, a file or directory with an access control list (POSIX, NFS
version 4, also as ZFS shows it, or rich, or a default one a directory
gives new files), whose entries the mode bits do not show, is refused like
one that differs. The
mandatory access control labels (SELinux and Smack) of the files and
directories compared must be the same too, or all absent: who may read
under a label is up to the system's policy. Publishing never sets a label.
Every file it stages or restores, and every directory it creates, must be
given that same label by the policy, which is checked before a byte is
written or the directory used; otherwise publishing fails and nothing is
published. A tree whose files and directories carry one label, as a user's
files commonly do under SELinux, can be published. A new file is staged
under another name, and renaming does not relabel it, so for a new file,
and for the empty file staged beside a deleted one, publishing first asks
SELinux (through `/sys/fs/selinux/create`) which labels the policy gives a
new file of the final name and one of the staging name, which may differ
by a rule keyed on the name, and refuses, before anything is staged,
unless they are the same, or if the policy cannot be asked; a process that
sets the label of what it makes (`setfscreatecon`) gives both that label. Smack's execution, mapping and transmuting labels
(`SMACK64EXEC`, `SMACK64MMAP`, `SMACK64TRANSMUTE`) are refused, since a
replacement, or a directory made again, would not get them.

A file whose bytes end as they began is not written. Restoring after a
failure gives every file back the mode and group it had. Other extended
attributes are not kept, nor on Windows other streams and attributes, nor
on macOS and Windows access control lists. Nothing beneath the root that
is a reparse point on Windows is used: a symbolic link, junction or mount
point, and also a cloud placeholder, a deduplicated file or one compressed
by the Windows overlay filter, is refused as a link when it is checked.
Loading refuses them too, so a tree in a OneDrive folder, whose files and
directories are cloud placeholders even when they are kept on the device,
cannot be loaded, previewed or applied on Windows. (A file NTFS compresses
is not a reparse point; its compression, an attribute, is not kept.) A
transaction that touches or consults two paths, or two directories on
their way, that differ only in case is refused with `tx.path`, since macOS and Windows, and some other file
systems, take them for one file; so is one with a path a component of
which ends in a dot or a space, which Windows drops from a name; on
Windows, one with a component that names a device (`CON`, `PRN`, `AUX`,
`NUL`, `COM0` to `COM9`, `LPT0` to `LPT9`, whatever its case and
extension); and so is
one that names a file, or a directory on its way, that the file system
finds by that name although it is named otherwise on disk (`TREE.jsonc`
for `tree.jsonc` where case is not told apart), which validation would
take for another file.

Each rename and removal is atomic, but there is no atomic publication of
several files. If a step of 4 fails, the files already published are
restored from the transaction's copy of them, each with the mode and group
it had, the staged files and created directories are removed, the
directories involved are synced, and the error names any file that could
not be restored. Whenever nothing is published, the cleanup is synced too.
An error always says three things: whether every file was published,
whether every directory it changed was synced, and whether the cleanup
finished, naming any staging file or directory it could not remove, to be
removed by hand. If the process stops during step 4 (a crash or power
loss), some files have the new bytes and the others the old ones, and
staging files may remain; the loader rejects a staging file below `planes/`
or `defs/`, so the tree fails validation until it is repaired. To repair
it, apply the inverse, which the command line writes before step 1: the
files the interrupted apply published are restored, the others already
match and are left alone, and the staging files beside every file it names
are removed. There is no journal: the inverse, written before anything is
published, is the recovery record, and applying it is a manual step. If
the recovery is interrupted in turn, apply the inverse again; a directory
the interrupted cleanup emptied may be left, which validation names where
it matters, to remove by hand. The
source tree's version control is the other way back.
Carrying out ignores whatever is at the staging name beside every file a
transaction touches or consults, a link or special file included.

Staging names, `.<name>.tilewright-tmp`, are reserved beside source files,
and `.tilewright.lock` in the root: on Windows it is the lock, and
publishing there refuses to take a lock file that no apply holds rather
than remove it.
A program other than the toolkit changing the tree at the same moment can
race a publish: the lock only orders applies, and on Windows such a
program can also redirect one by swapping a link in. Windows cannot sync a
directory, so a publish there reports that it was not made durable, and
the command line's writes of its outputs say that their names may not be
durable yet.

## 10. Canonical layout

Text the toolkit creates, new files and new values, uses two-space
indentation and `"key": value` members with minimal string escaping. A
container is written on one line when it is empty, when it is an array of
numbers, booleans and nulls only whose one-line form is at most 4,096
bytes, or when its one-line form is at most 80 bytes; otherwise each item
is on its own line one level deeper; a string value, item or key longer
than 4,096 bytes is on a line of its own, from the line's first column. A
file's root container is always on several lines, an empty one on two, and
the file ends with a newline.

## 11. Limits

Transactions are untrusted input. They are parsed under the JSONC limits of
FORMAT.md section 3 and these, while they are read and carried out:

| Limit | Value |
| --- | ---: |
| Operations | 100,000 |
| Files touched or consulted, and `base` or `result` entries | 10,000 |
| Pointer steps | 64 |
| Index in a pointer or address | 250,000 |
| Terrain cells | 1,000,000 |
| Bytes read from touched files | 256 MiB |
| Work, in bytes: each operation's own size, the size of each document it reads (once per operation) and of each edit it makes | 4 GiB |
| Diagnostics reported, the edited world's included, the last place kept for a note that there are more | 1,000 |

Whatever an operation does within a document, finding values, scanning
members or objects and comparing values, is bounded by a constant times
the work it is charged, so no lookup is metered on its own. A transaction
with uid addresses also loads the tree once, within the load's own bounds
as validating does; a uid lookup reads the chunk that holds the object and
each chunk changed since the last lookup, as documents the operation reads. A file is read
only as far as the read allowance left; the inverse's text and operations
are counted as operations run and must fit a transaction; one terrain operation names at
most 10,000 chunks; and carrying out stops as soon as a budget is spent or
the diagnostic limit is reached. Messages describe a value over 4 KiB by its kind and size
rather than render it.

Each touched file must be within the JSONC limits, as read and after every
edit; an operation that finds or leaves one past a limit fails with
`tx.limit`. A file larger than the 64 MiB document limit, or than the read
allowance left, is not read: that is a `tx.limit` too, and carrying out
stops there. A file a transaction deletes must be valid JSONC, so that its
inverse can create it again.
The sealed transaction and the inverse must themselves be valid transactions
under these limits; a transaction whose inverse would not be is rejected
before anything is written, and should be split.

## 12. Diagnostics

Problems of a transaction are reported at byte spans of its document, with
these codes; problems of the edited world use the loader's codes (FORMAT.md
section 14) at the edited files.

| Code | Meaning |
| --- | --- |
| `jsonc.*` | The document is not strict JSONC. |
| `tx.shape` | The document's shape: an unknown key or operation, a missing or mistyped value. |
| `tx.conflict` | A precondition failed (section 7), or the files changed while applying. |
| `tx.result` | Replaying the transaction gave other bytes than its `result`. |
| `tx.path` | A path no transaction may touch, a symbolic link in the way, or, when publishing, a path a file system could take for another's (section 9). |
| `tx.limit` | A limit of section 11. |
| `tx.io` | Reading or writing failed, or publishing is refused on this platform or for the access of the files (section 9). |

## 13. Command line

```sh
tilewright tx preview [--json] TRANSACTION ROOT
tilewright tx rebase TRANSACTION ROOT OUTPUT
tilewright tx apply [--unchecked-access] TRANSACTION ROOT INVERSE
```

`preview` carries the transaction out and prints the files it would create
(`A`), modify (`M`) and delete (`D`), or with `--json` an object with each
file's change and digests. `rebase` writes the transaction replayed on the
tree as it is to `OUTPUT`. `apply` writes the inverse to `INVERSE`, then
publishes. Neither replaces an existing file. Inside the source tree,
neither writes:

- into `world.jsonc`, `profile/`, `planes/` or `defs/`;
- at `.world.jsonc.tilewright-tmp`, where publishing stages `world.jsonc`,
  or at `.tilewright.lock`, the lock on Windows;
- at a name that a file system could take for one of those: names are
  compared without case and without trailing dots and spaces, and a path
  with a component that is not ASCII, which a file system may fold by
  rules of its own, or that holds a colon, which on Windows names a stream
  of a file, is refused anywhere in the tree.

Where publishing does not check everything (section 9), `apply` refuses
before writing anything, the inverse included, unless `--unchecked-access`
accepts that; it then warns on standard error, naming what is not checked.

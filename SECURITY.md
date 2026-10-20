# Security policy

Report vulnerabilities privately through GitHub's security advisory interface.

Source documents and content packs are untrusted input. Parsers enforce size,
depth and count limits while reading, reject malformed data with stable error
codes, and never execute content. Checking an object against a profile's
schema costs time in proportion to the fields the object has, however many
fields the schema requires. Tools write only to explicit output paths and
never overwrite source files except through validated span edits. The CLI
escapes control characters, newlines included, in every untrusted value it
prints, so content from a source or pack cannot send escape sequences to the
terminal or add lines to the output.

Loading a source tree never follows a symbolic link beneath its root (on
Windows, any reparse point, junctions included). The root path itself is
resolved as the caller gives it, so it may be, or pass through, a link. On
Unix and Windows this holds even if the tree changes during the load,
because every component beneath the root is opened through its parent
directory's handle. On other platforms components are checked by path
first, which a local process changing the tree at the same time can race.
Sources are local checkouts, so a race needs a local attacker.

Applying a transaction writes, renames and removes files beneath the root
through the same confinement, on Linux, macOS and Windows: there every
operation goes through the directory handles opened while the transaction
was carried out, so changing the tree cannot redirect it. A transaction
moves content between the files it changes, so a source tree is one unit
of access: publishing requires the files it changes and every directory
above them to be reachable by the same users, refuses access control it
could not keep, and never gives a file it writes access the files it
replaces did not have.

- On Unix, the classes that may read each file and search each directory,
  and the group where only it may, must be the same, and no file or
  directory may have an access control list. Written files are given that
  mode and group.
- On Linux, their mandatory access control labels (SELinux, Smack) must be
  the same too. Publishing never sets a label: the system's policy must give
  every file it writes, and directory it creates, that label, which is
  checked before a byte is written. For a new file, which is staged under
  another name, SELinux is also asked which label the policy gives its final
  name, and publishing refuses if that differs or SELinux cannot be asked.
  Smack's execution, mapping and transmuting labels are refused.
- On Windows, the changed files must share one access control list, and the
  directories above them one, and every file publishing writes and every
  directory it creates must be given that list by its directory, which is
  checked before a byte is written; so a file with entries of its own, which
  a replacement would not get, is refused, and so is a list with an entry
  for OWNER RIGHTS, since the owner changes. Publishing never changes a
  list. A list protected from inheritance, a mandatory integrity label, a
  central access policy, a resource attribute, a trust label, encryption or
  a read-only file is refused. The lock is a file in the root,
  `.tilewright.lock`, that is shared with no one and deleted when released.

Every file a transaction deletes must be one its inverse can make again:
an empty file is staged beside it, as the inverse would make it, and must
get the access it had.

Other platforms, the BSDs other than macOS among them, are not checked for
access control lists and labels the same way, so applying is refused
there; previewing and rebasing work everywhere. The access checks are made
before anything is written and again just before the first rename, but as
with verifying packs, applying transactions in a tree that others can
write to is not supported: such a writer could swap a file between a check
and its use. The low-level calls Rust's standard library lacks for this, on
Windows and macOS, are in `tilewright-platform`, the only crate that may
use `unsafe`.

A pack's file name digest is checked against the file at its path, so the
check binds to the path. Packs are content-addressed files opened from
directories the caller controls; verifying packs in a directory others can
write to is not supported. Verification decodes every chunk and stops once a
caller-set decoded-byte budget (8 GiB by default) is passed. A pack file
longer than a caller-set size limit (8 GiB by default) is rejected before it
is opened, and hashing stops at that limit. A pack has no free pages, and a
full verification's integrity check reads every page before the file is
hashed, so a sparse file cannot hide holes that only the hash reads.

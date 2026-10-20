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

Loading a source tree never follows a symbolic link beneath its root. The
root path itself is resolved as the caller gives it, so it may be, or pass
through, a link. On Unix this holds even if the tree changes during the load,
because every component beneath the root is opened through its parent
directory's handle. On other platforms components are
checked by path first, which a local process changing the tree at the same
time can race. Sources are local checkouts, so a race needs a local attacker.

Applying a transaction writes, renames and removes files beneath the root
through the same confinement, on Linux only: there every operation goes
through the directory handles opened while the transaction was carried out,
so changing the tree cannot redirect it. A transaction moves content
between the files it changes, so a source tree is one unit of access:
publishing requires the files it changes and every directory above them to
be reachable by the same users (the classes that may read each file and
search each directory, and the group where only it may) and to have no
access control list or mandatory access control label (SELinux, Smack),
refuses otherwise, and gives the files it writes that same access. Other platforms either cannot reach files through
handles or cannot be checked for access control lists the same way, so
applying is refused there; previewing and rebasing work everywhere. As
with verifying packs, applying transactions in a tree that others can
write to is not supported: such a writer could swap a file between a check
and its use.

A pack's file name digest is checked against the file at its path, so the
check binds to the path. Packs are content-addressed files opened from
directories the caller controls; verifying packs in a directory others can
write to is not supported. Verification decodes every chunk and stops once a
caller-set decoded-byte budget (8 GiB by default) is passed. A pack file
longer than a caller-set size limit (8 GiB by default) is rejected before it
is opened, and hashing stops at that limit. A pack has no free pages, and a
full verification's integrity check reads every page before the file is
hashed, so a sparse file cannot hide holes that only the hash reads.

The browser package reads a pack from bytes the caller gives it. It copies
them once, after checking their length against the pack size limit, into a
read-only database in WebAssembly memory, so later changes to the caller's
buffer cannot reach the reader, and an expected digest binds to the copy it
reads. Its JavaScript layer checks every argument's type and range before
the reader sees it. If the module traps, for example on running out of
memory, it refuses every later call rather than continue in an unknown
state.

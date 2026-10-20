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
so changing the tree cannot redirect it, and a file with an access control
list (POSIX, NFS version 4 or rich), whose mode bits do not say who may
read it, is refused rather than replaced, removed or copied from. A
replaced file keeps the mode bits of the file it replaces, or narrower
ones, and drops any access control list its directory gives new files, so
the mode bits alone say who may read it; other extended attributes and the
owner are not carried over. Other
platforms either cannot reach files through handles or cannot be checked
for access control lists the same way, so applying is refused there;
previewing and rebasing work everywhere.

A pack's file name digest is checked against the file at its path, so the
check binds to the path. Packs are content-addressed files opened from
directories the caller controls; verifying packs in a directory others can
write to is not supported. Verification decodes every chunk and stops once a
caller-set decoded-byte budget (8 GiB by default) is passed. A pack file
longer than a caller-set size limit (8 GiB by default) is rejected before it
is opened, and hashing stops at that limit. A pack has no free pages, and a
full verification's integrity check reads every page before the file is
hashed, so a sparse file cannot hide holes that only the hash reads.

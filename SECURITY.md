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
Unix this holds even if the tree changes during the load, because every
component beneath the root is opened through its parent directory's
handle. On other platforms components are checked by path first, which a
local process changing the tree at the same time can race. Sources are
local checkouts, so a race needs a local attacker.

Applying a transaction writes, renames and removes files beneath the root
through the same confinement. A transaction moves content between the files
it changes, so a source tree is one unit of access: publishing requires the
files it changes and every directory above them to be reachable by the same
users (the classes that may read each file and search each directory, the
group where only it may, and the owner where not everyone may, refusing a
mode that lets others read what it denies the group or the owner), gives the
files it writes that same access, and deletes a file only if its inverse
would make it again for the same users: an empty file is staged beside it as
the inverse would make it, and checked. What else holds depends on the
platform:

- **Linux** keeps every guarantee. Every operation goes through the
  directory handles opened while the transaction was carried out, so
  changing the tree cannot redirect it. A file or directory with an access
  control list (POSIX, NFS version 4 or rich) is refused, since a
  replacement could not keep it. That holds on the local file systems
  that keep their lists as the attributes publishing reads (ext2, ext3,
  ext4, XFS, Btrfs, tmpfs, F2FS, ZFS and bcachefs). On any other, Linux
  may not show the lists at all (network, host-shared, FUSE, NTFS and
  stacked ones among them, such as WSL's mounts of Windows drives, a
  virtual machine's 9p or shared folders, or a container's overlay root),
  so there applying needs the same explicit consent as on macOS and
  Windows. A directory's file system is told by both the type of its mount
  in `/proc/self/mountinfo`, found by mount ID, and the magic number
  `statfs` gives, since a 9p share passes on its host's magic number.
  Wherever it cannot be told, consent is needed too, and the refusal says
  why. For example: without a readable `/proc` (some hardened containers,
  a chroot without one), on kernels before 3.15, which give no mount ID,
  where the mount's own line is malformed, or where the mount is not listed
  at all, as in a chroot whose root is not a mount point (the kernel lists
  only mounts reachable from the root, so the one holding the tree may be
  missing even with `/proc` mounted) or for a mount made after the list
  was read.
  Each line is read on its own, so another user's mount cannot make every
  tree need consent. In a container, keep the tree on a bind-mounted local
  volume rather than the overlay root.
  The mandatory access control labels (SELinux, Smack) of the entries compared
  must be the same, and the system's policy must give every file publishing
  writes, and directory it creates, that label, which is checked before a
  byte is written; publishing never sets a label. For a new file, which is
  staged under another name, SELinux is also asked which label the policy
  gives its final name, and publishing refuses if that differs or SELinux
  cannot be asked. Smack's execution, mapping and transmuting labels are
  refused.
- **macOS** reaches files through directory handles too, so a link swapped
  in is never followed. But access control lists are neither checked nor
  kept: a replaced file, or one an inverse makes again, takes the access a
  new file there gets, so a file whose list narrows who may read it loses
  that list.
- **Windows** reaches files by path, after checking each component for a
  reparse point (a symbolic link, junction or mount point, or any other), so
  a local process that swaps a link in between the check and the use can
  redirect a write, rename or removal outside the tree. Loading refuses
  reparse points too, so a tree in a OneDrive folder, whose files are
  cloud placeholders even when kept on the device, cannot be loaded or
  applied there. Access control lists
  are neither checked nor kept: a replaced file, or one an inverse makes
  again, takes the list its directory gives a new file. A read-only file,
  and an encrypted (EFS) file or directory, is refused. The lock is a file
  in the root, `.tilewright.lock`, shared with no one and deleted when
  released.

So on macOS and Windows, and on Linux on any other file system, applying
transactions is supported only in a tree that no one else can change and
in which no file or directory has an access control list of its own.
Nothing there checks that, so applying is refused by default and goes
ahead only when the caller accepts it explicitly:
`PublishOptions::unchecked_access` in the library, `tilewright tx apply
--unchecked-access` on the command line, which then warns, naming what is
not checked. On Linux on the local file systems above, which checks
everything, no such consent is needed. On other platforms, the BSDs among them, applying is
refused; previewing and rebasing work everywhere. On every
platform the access checks are made before anything is written and again
just before the first rename, but as with verifying packs, applying
transactions in a tree that others can write to is not supported: such a
writer could swap a file between a check and its use.

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
the reader sees it. Its verification runs on the calling thread, so its
decoded-byte budget is 256 MiB by default rather than 8 GiB, and a single
read builds values for at most 65,536 of a chunk's objects and fields, or
of a stairwell's steps, by default, failing before it builds any past the
budget. If a call into the module fails other than by
rejecting a pack, as on a trap from running out of memory, it refuses every
later call rather than continue in an unknown state, and frees nothing into
it.

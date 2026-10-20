// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Writing a plan: under the root's lock, stage every new file beside its
//! target, check the files and validate the result again, then rename the
//! staged files into place and remove deleted ones. A failure while
//! publishing restores the files already published from the plan's copies.

use crate::{Code, FileChange, Plan, diagnostic, digest, staging_name, staging_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
use tilewright_source::{Access, AccessError, Diagnostic, SourceDirectory, SourceRoot};

#[cfg(test)]
thread_local! {
    /// Fails the publish step with this index, for tests.
    pub(crate) static FAIL_STEP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Runs after staging and before the files are checked again, for tests.
    pub(crate) static BEFORE_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
    /// Fails every directory sync, for tests.
    pub(crate) static FAIL_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Why publishing did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishError {
    /// Whether every file was published, so the tree holds the
    /// transaction's result. When not, it holds the files as they were,
    /// except any the problems name as not restored.
    pub applied: bool,
    /// Whether every directory the publish changed was synced.
    pub durable: bool,
    /// Whether the cleanup finished: no staging file or emptied directory
    /// was left that should have gone. One left is named in the problems,
    /// to remove by hand; a staging file left below `planes/` or `defs/`
    /// keeps the tree from validating.
    pub cleaned: bool,
    pub problems: Vec<Diagnostic>,
}

impl PublishError {
    /// The outcome in words, such as "the transaction was applied, but not
    /// made durable".
    #[must_use]
    pub fn summary(&self) -> String {
        let mut summary = if self.applied {
            "the transaction was applied".to_owned()
        } else {
            "the transaction was not applied".to_owned()
        };
        let mut gaps = Vec::new();
        if !self.durable {
            gaps.push("not made durable");
        }
        if !self.cleaned {
            gaps.push("its cleanup is incomplete");
        }
        if !gaps.is_empty() {
            summary.push_str(if self.applied { ", but " } else { ", and " });
            summary.push_str(&gaps.join(" and "));
        }
        summary
    }
}

impl fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.summary())?;
        for problem in &self.problems {
            write!(formatter, "; {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PublishError {}

/// A finished publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Published {
    /// Whether every changed directory was synced. It is `false` only on
    /// platforms that cannot sync a directory: the files are synced, but
    /// their renames and removals may not be durable yet.
    pub durable: bool,
}

fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn problem(path: &str, code: Code, message: impl Into<String>) -> Vec<Diagnostic> {
    vec![diagnostic::file(path, code.as_str(), message)]
}

fn io(path: &str, error: &AccessError) -> Vec<Diagnostic> {
    let code = match error {
        AccessError::Io(..) | AccessError::TooLarge(_) => Code::Io,
        _ => Code::Path,
    };
    problem(path, code, error.to_string())
}

fn not_applied(problems: Vec<Diagnostic>) -> PublishError {
    PublishError {
        applied: false,
        durable: true,
        cleaned: true,
        problems,
    }
}

/// Whether publishing is supported here: only on Linux are files reached
/// through directory handles and checked for access control lists, which a
/// replacement could not keep.
pub const PUBLISH_SUPPORTED: bool = cfg!(target_os = "linux");

/// Why publishing is refused where it is not supported.
const UNSUPPORTED: &str = "applying a transaction is supported only on Linux, where files are reached through directory handles and checked for access control lists; preview and rebase work everywhere";

/// How many directory handles a publish keeps open at once; past it, one
/// is closed and opened again when needed, so that a transaction across
/// many directories does not run out of file descriptors.
pub(crate) const OPEN_DIRECTORIES: usize = 64;

/// The directories of a publish, the most recent kept open.
struct Directories<'r> {
    root: &'r SourceRoot,
    open: BTreeMap<String, SourceDirectory>,
    /// Directories this publish created, in creation order.
    created: Vec<String>,
    /// Directories whose entries changed, to sync.
    changed: BTreeSet<String>,
    /// Cleanup that could not be done, and how many more past the limit.
    leftovers: Vec<Diagnostic>,
    dropped: usize,
}

impl Directories<'_> {
    /// The directory `path`. With `create`, its missing parents and itself
    /// are made, each with that access.
    fn get(
        &mut self,
        path: &str,
        create: Option<&Access>,
    ) -> Result<Option<&SourceDirectory>, AccessError> {
        if !self.open.contains_key(path) {
            if let Some(access) = create {
                let mut prefix = String::new();
                for component in path.split('/').filter(|component| !component.is_empty()) {
                    if !prefix.is_empty() {
                        prefix.push('/');
                    }
                    prefix.push_str(component);
                    // Only a directory this call made is this publish's to
                    // remove again; one another writer made meanwhile is not.
                    if self.root.directory(&prefix, false)?.is_none()
                        && self.root.make_directory(&prefix)?
                    {
                        self.created.push(prefix.clone());
                        self.changed.insert(split(&prefix).0.to_owned());
                        if let Some(made) = self.root.directory(&prefix, false)? {
                            made.set_own_access(access)?;
                        }
                    }
                }
            }
            match self.root.directory(path, false)? {
                Some(directory) => {
                    if self.open.len() >= OPEN_DIRECTORIES {
                        self.open.pop_first();
                    }
                    self.open.insert(path.to_owned(), directory);
                }
                None => return Ok(None),
            }
        }
        Ok(self.open.get(path))
    }

    /// Removes the file, or the empty directory, at `path`; `false` when
    /// there was none, or the directory is not empty. A removal records its
    /// parent as changed, to sync.
    fn remove(&mut self, path: &str, directory: bool) -> Result<bool, AccessError> {
        let (parent, name) = split(path);
        let removed = match self.get(parent, None)? {
            Some(handle) if directory => handle.remove_directory(name)?,
            Some(handle) => handle.remove_file(name)?,
            None => false,
        };
        if removed {
            self.changed.insert(parent.to_owned());
            if directory {
                self.open.remove(path);
                self.changed.remove(path);
            }
        }
        Ok(removed)
    }

    /// Cleans up the file, or empty directory, at `path`. A failure is kept
    /// as a leftover to report; returns whether it was removed.
    fn clean(&mut self, path: &str, directory: bool) -> bool {
        match self.remove(path, directory) {
            Ok(removed) => removed,
            Err(error) => {
                self.leftover(diagnostic::file(
                    path,
                    Code::Io.as_str(),
                    format!("could not be removed ({error}); remove it by hand"),
                ));
                false
            }
        }
    }

    /// Keeps a leftover, up to the diagnostic limit; past it, the rest are
    /// counted.
    fn leftover(&mut self, problem: Diagnostic) {
        if self.leftovers.len() < crate::limits::DIAGNOSTICS {
            self.leftovers.push(problem);
        } else {
            self.dropped += 1;
        }
    }

    /// Removes the directories this publish created. One that is not
    /// empty now, since another writer added to it, is reported.
    fn remove_created(&mut self) {
        for path in std::mem::take(&mut self.created).into_iter().rev() {
            let removed = match self.remove(&path, true) {
                Ok(true) => continue,
                Ok(false) => matches!(self.root.directory(&path, false), Ok(None)),
                Err(error) => {
                    self.leftover(diagnostic::file(
                        &path,
                        Code::Io.as_str(),
                        format!("could not be removed ({error}); remove it by hand"),
                    ));
                    continue;
                }
            };
            if !removed {
                self.leftover(diagnostic::file(
                    &path,
                    Code::Io.as_str(),
                    "this apply created the directory and could not remove it, since it is not empty; remove it by hand if it is not wanted",
                ));
            }
        }
    }

    /// Removes the directories above a deleted file that are now empty, up
    /// to the top-level directories. A directory that is not empty is left,
    /// as it should be.
    fn prune(&mut self, path: &str) {
        let mut current = split(path).0.to_owned();
        while current.contains('/') && self.clean(&current, true) {
            current = split(&current).0.to_owned();
        }
    }

    /// The outcome: success when every file was published and nothing went
    /// wrong, otherwise an error with every problem, the cleanup left and
    /// the directories that could not be synced.
    fn outcome(
        &mut self,
        applied: bool,
        mut problems: Vec<Diagnostic>,
    ) -> Result<Published, PublishError> {
        let cleaned = self.leftovers.is_empty();
        problems.append(&mut self.leftovers);
        let limit = crate::limits::DIAGNOSTICS;
        let (failures, unsynced, platform) = self.sync(limit.saturating_sub(problems.len()));
        let durable = unsynced == 0 && platform;
        let dropped = self.dropped + (unsynced - failures.len());
        problems.extend(failures);
        // Every problem counts toward the limit; past it, the last place
        // goes to one that says how many more there were.
        if dropped + problems.len() > limit {
            let more = dropped + problems.len() - (limit - 1);
            problems.truncate(limit - 1);
            problems.push(diagnostic::file(
                ".",
                Code::Limit.as_str(),
                format!("{more} more problem(s) are not reported"),
            ));
        }
        if applied && problems.is_empty() {
            Ok(Published { durable: platform })
        } else {
            Err(PublishError {
                applied,
                durable,
                cleaned,
                problems,
            })
        }
    }

    /// Syncs every directory whose entries changed. Returns up to `room`
    /// of the failures, how many there were, and whether the platform could
    /// sync every directory.
    fn sync(&mut self, room: usize) -> (Vec<Diagnostic>, usize, bool) {
        let mut failures = Vec::new();
        let mut failed = 0;
        let mut durable = true;
        for path in std::mem::take(&mut self.changed) {
            let failure = self.sync_one(&path, &mut durable);
            if let Some(failure) = failure {
                failed += 1;
                if failures.len() < room {
                    failures.extend(failure);
                }
            }
        }
        (failures, failed, durable)
    }

    /// Syncs one directory; returns why it failed, if it did.
    fn sync_one(&mut self, path: &str, durable: &mut bool) -> Option<Vec<Diagnostic>> {
        #[cfg(test)]
        if FAIL_SYNC.with(std::cell::Cell::get) {
            return Some(problem(path, Code::Io, "injected sync failure"));
        }
        match self.get(path, None) {
            Ok(Some(directory)) => match directory.sync() {
                Ok(synced) => {
                    *durable &= synced;
                    None
                }
                Err(error) => Some(io(path, &error)),
            },
            Ok(None) => None,
            Err(error) => Some(io(path, &error)),
        }
    }
}

/// What publishing read of the files it changes and of the directories
/// that hold them, checked again just before anything is renamed.
#[derive(Default)]
struct Known {
    /// Each changed file as it was, `None` where there was none: a file
    /// written there keeps this access, and a failed publish restores it.
    files: BTreeMap<String, Option<Access>>,
    /// Every directory there was above each changed file, up to the root.
    /// A new file takes the mode of the nearest, without the execute bits,
    /// and its group, and a new directory its mode and group.
    directories: BTreeMap<String, Access>,
}

impl Known {
    /// The access a new file at `path` gets, from the nearest directory
    /// above it that was there.
    fn new_file(&self, path: &str) -> Option<Access> {
        let mut directory = split(path).0;
        loop {
            if let Some(access) = self.directories.get(directory) {
                return Some(Access {
                    mode: access.mode & 0o666,
                    ..*access
                });
            }
            if directory.is_empty() {
                return None;
            }
            directory = split(directory).0;
        }
    }

    /// The access the directories made for a file at `path` get.
    fn new_directory(&self, path: &str) -> Option<&Access> {
        let mut directory = split(path).0;
        loop {
            if let Some(access) = self.directories.get(directory) {
                return Some(access);
            }
            if directory.is_empty() {
                return None;
            }
            directory = split(directory).0;
        }
    }
}

/// Who may reach content, by an access: for a file the classes that may
/// read it, for a directory those that may search it, as read bits, and the
/// group where it says who may.
fn readers(access: &Access, directory: bool) -> (u32, Option<u32>) {
    let reach = if directory {
        (access.mode & 0o111) << 2
    } else {
        access.mode & 0o444
    };
    (
        reach,
        (reach & 0o040 != 0 && reach & 0o004 == 0).then_some(access.group),
    )
}

fn shown(path: &str) -> &str {
    if path.is_empty() { "." } else { path }
}

/// Reads the access of every file the plan changes and of every directory
/// above each, up to the root, and checks that they all have the same
/// readers. A transaction moves content between the files it changes, a
/// renamed file's or a moved object's, and its inverse recreates deleted
/// files, and directories pruned since, with the access of the directory
/// above them; so no file may be readable by anyone another is not.
fn survey(
    directories: &mut Directories<'_>,
    changed: &[&FileChange],
) -> Result<Known, Vec<Diagnostic>> {
    let mut known = Known::default();
    for file in changed {
        let (parent, name) = split(&file.path);
        let existing = match directories.get(parent, None) {
            Ok(Some(directory)) => directory.access(name),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        }
        .map_err(|error| io(&file.path, &error))?;
        known.files.insert(file.path.clone(), existing);
        // Each directory above, up to the root or one already read.
        let mut directory = parent;
        while !known.directories.contains_key(directory) {
            let found = match directories.get(directory, None) {
                Ok(Some(handle)) => handle.own_access().map(Some),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            }
            .map_err(|error| io(shown(directory), &error))?;
            if let Some(access) = found {
                known.directories.insert(directory.to_owned(), access);
            }
            if directory.is_empty() {
                break;
            }
            directory = split(directory).0;
        }
    }
    let mut audiences = known
        .files
        .iter()
        .filter_map(|(path, access)| Some((path.as_str(), readers(access.as_ref()?, false))))
        .chain(
            known
                .directories
                .iter()
                .map(|(path, access)| (shown(path), readers(access, true))),
        );
    if let Some((first, expected)) = audiences.next()
        && let Some((other, _)) = audiences.find(|(_, found)| *found != expected)
    {
        return Err(problem(
            other,
            Code::Io,
            format!(
                "{other} and {first} can be reached by different users; the files a transaction changes must be readable, and the directories above them searchable, by the same classes and, where the group alone may, the same group"
            ),
        ));
    }
    Ok(known)
}

/// Checks that the access publishing read is still what is on disk, so
/// that no file is published with access that changed since.
fn check_access(directories: &mut Directories<'_>, known: &Known) -> Result<(), Vec<Diagnostic>> {
    let changed = |path: &str| {
        problem(
            shown(path),
            Code::Conflict,
            "its permissions or owner changed while the transaction was being applied; nothing was written",
        )
    };
    for (path, before) in &known.files {
        let (parent, name) = split(path);
        let now = match directories.get(parent, None) {
            Ok(Some(directory)) => directory.access(name),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        }
        .map_err(|error| io(path, &error))?;
        if now != *before {
            return Err(changed(path));
        }
    }
    for (path, before) in &known.directories {
        let now = match directories.get(path, None) {
            Ok(Some(directory)) => directory.own_access().map(Some),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        }
        .map_err(|error| io(shown(path), &error))?;
        if now.as_ref() != Some(before) {
            return Err(changed(path));
        }
    }
    Ok(())
}

/// Stages every new file: one written where a file is with that file's
/// access, a new one with its directory's. On failure removes every
/// staging file it wrote, the failed one included, and the directories it
/// created.
fn stage_all<'f>(
    directories: &mut Directories<'_>,
    known: &Known,
    changed: &[&'f FileChange],
) -> Result<Vec<&'f str>, PublishError> {
    let mut staged = Vec::new();
    for file in changed {
        let Some(text) = &file.after else {
            continue;
        };
        staged.push(file.path.as_str());
        // Writing removes whatever was at the staging name first, so the
        // directory may change even if the write fails.
        directories.changed.insert(split(&file.path).0.to_owned());
        let (parent, name) = split(&file.path);
        let access = match known.files.get(&file.path) {
            Some(Some(access)) => Some(*access),
            _ => known.new_file(&file.path),
        };
        let result = directories
            .get(parent, known.new_directory(&file.path))
            .and_then(|directory| {
                let directory =
                    directory.ok_or_else(|| AccessError::Unexpected(file.path.clone()))?;
                directory.write(&staging_name(name), access.as_ref(), text.as_bytes())?;
                // The new inode must have no access beyond its mode bits,
                // such as a label the system's policy gave it.
                directory.access(&staging_name(name)).map(|_| ())
            });
        if let Err(error) = result {
            discard(directories, &staged);
            directories.remove_created();
            return Err(directories
                .outcome(false, io(&file.path, &error))
                .err()
                .unwrap_or_else(|| not_applied(Vec::new())));
        }
    }
    Ok(staged)
}

/// Checks that every file is as it was planned.
fn check_again(root: &SourceRoot, files: &[FileChange]) -> Result<(), Vec<Diagnostic>> {
    let limit =
        u64::try_from(tilewright_jsonc::Limits::default().max_input_bytes).unwrap_or(u64::MAX);
    for file in files {
        let changed = || {
            problem(
                &file.path,
                Code::Conflict,
                "the file changed while the transaction was being applied; nothing was written",
            )
        };
        let current = match root.read(&file.path, limit) {
            Ok(current) => current,
            // A file grown past what can be read, or replaced by a link,
            // a directory or a special file, changed too.
            Err(AccessError::TooLarge(_) | AccessError::Link(_) | AccessError::Unexpected(_)) => {
                return Err(changed());
            }
            Err(error) => return Err(io(&file.path, &error)),
        };
        let found = current.map(|bytes| {
            use sha2::Digest as _;
            <[u8; 32]>::from(sha2::Sha256::digest(&bytes))
        });
        if found != digest(file.before.as_deref()) {
            return Err(changed());
        }
    }
    Ok(())
}

impl Plan {
    /// Validates the tree with the plan's files again, and its terrain
    /// addresses against it, since files the plan did not read may have
    /// changed since it was made.
    fn validate_again(&self, root: &SourceRoot) -> Result<(), Vec<Diagnostic>> {
        let world = tilewright_source::load_edited_in(root, &self.load, &self.overlay())?;
        let limit = crate::limits::DIAGNOSTICS;
        let mut problems: Vec<Diagnostic> = self
            .terrain
            .iter()
            .filter_map(|(plane, grid, at)| {
                crate::terrain_problem(&world, plane, grid).map(|message| Diagnostic {
                    message,
                    ..at.clone()
                })
            })
            .take(limit)
            .collect();
        // The last place goes to a note that there are more.
        if problems.len() == limit {
            problems.truncate(limit - 1);
            let name = problems[0].path.clone();
            problems.push(diagnostic::file(
                &name,
                Code::Limit.as_str(),
                format!(
                    "more than {} problems; the rest are not reported",
                    limit - 1
                ),
            ));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// Writes the plan's files. The root's lock is held throughout. Every
    /// file the plan touched or consulted must still be as it was planned,
    /// and the tree with the plan's files must still validate, since files
    /// the plan did not touch may have changed since it was made.
    ///
    /// # Errors
    /// Returns a [`PublishError`]. With nothing written: a `tx.conflict`
    /// diagnostic when a file changed or another edit holds the lock, the
    /// loader's diagnostics when the result no longer validates, and `tx.io`
    /// or `tx.path` ones when staging fails. A failure while renaming
    /// restores the files already published and names any it could not.
    /// Cleanup that could not be done and directories that could not be
    /// synced are reported too, with `cleaned` or `durable` unset, whether
    /// or not every file was published. Where [`PUBLISH_SUPPORTED`] is
    /// false, publishing is refused with a `tx.io` diagnostic and nothing is
    /// written.
    pub fn publish(&self) -> Result<Published, PublishError> {
        if !PUBLISH_SUPPORTED {
            return Err(not_applied(problem(".", Code::Io, UNSUPPORTED)));
        }
        let root = &self.source;
        let _lock = root.lock().map_err(|error| {
            not_applied(match error {
                AccessError::Locked => problem(".", Code::Conflict, error.to_string()),
                other => io(".", &other),
            })
        })?;
        let mut directories = Directories {
            root,
            open: BTreeMap::new(),
            created: Vec::new(),
            changed: BTreeSet::new(),
            leftovers: Vec::new(),
            dropped: 0,
        };
        // Files are written first and removed last, so that whatever stops
        // a publish halfway, a renamed file is never only removed.
        let mut changed: Vec<&FileChange> =
            self.files.iter().filter(|file| file.is_changed()).collect();
        changed.sort_by_key(|file| file.after.is_none());
        let known = match survey(&mut directories, &changed) {
            Ok(known) => known,
            Err(problems) => return directories.outcome(false, problems),
        };
        let staged = stage_all(&mut directories, &known, &changed)?;

        #[cfg(test)]
        if let Some(hook) = BEFORE_CHECK.with(|hook| hook.borrow_mut().take()) {
            hook();
        }

        let checked = check_again(root, &self.files).and_then(|()| self.validate_again(root));
        // Last, as close to publishing as can be: the access the staged
        // files were given must still be what it came from.
        let checked = checked.and_then(|()| check_access(&mut directories, &known));
        if let Err(problems) = checked {
            discard(&mut directories, &staged);
            directories.remove_created();
            return directories.outcome(false, problems);
        }
        let mut published: Vec<&FileChange> = Vec::new();
        for (step, file) in changed.iter().enumerate() {
            let (parent, name) = split(&file.path);
            let result = publish_one(
                &mut directories,
                parent,
                name,
                file.after.is_some(),
                step,
                &mut published,
                file,
            );
            if let Err(error) = result {
                let unrestored = roll_back(&mut directories, &known, &published);
                discard(&mut directories, &staged);
                directories.remove_created();
                let message = if unrestored.is_empty() {
                    format!("{error}; every file published before it was restored")
                } else {
                    format!(
                        "{error}; restoring failed for: {}; apply the inverse to restore the rest",
                        diagnostic::listed(unrestored.into_iter())
                    )
                };
                return directories.outcome(false, problem(&file.path, Code::Io, message));
            }
        }
        // Staging files left beside the files of this plan by an earlier,
        // interrupted apply go too, and the directories deleting files
        // emptied.
        for file in &self.files {
            // Beside a file that is not there, such a staging file may have
            // been all its directories held.
            if directories.clean(&staging_path(&file.path), false) && file.after.is_none() {
                directories.prune(&file.path);
            }
        }
        for file in &changed {
            if file.after.is_none() {
                directories.prune(&file.path);
            }
        }
        directories.outcome(true, Vec::new())
    }
}

/// Publishes one file: renames its staged text into place, or removes it.
/// The file is recorded as published as soon as its target changed.
fn publish_one<'f>(
    directories: &mut Directories<'_>,
    parent: &str,
    name: &str,
    write: bool,
    step: usize,
    published: &mut Vec<&'f FileChange>,
    file: &'f FileChange,
) -> Result<(), AccessError> {
    #[cfg(test)]
    if FAIL_STEP.with(std::cell::Cell::get) == Some(step) {
        return Err(AccessError::Io(
            name.to_owned(),
            std::io::Error::other("injected failure"),
        ));
    }
    let _ = step;
    if write {
        let Some(directory) = directories.get(parent, None)? else {
            return Err(AccessError::Unexpected(parent.to_owned()));
        };
        directory.rename(&staging_name(name), name)?;
    } else {
        // A staging file left by an interrupted apply goes first, so that
        // failing to remove it leaves the target untouched; its removal is
        // recorded to sync whatever happens next. The target's access was
        // noted, and checked, before publishing began.
        if let Err(error) = directories.remove(&staging_path(&file.path), false) {
            directories.leftover(diagnostic::file(
                &staging_path(&file.path),
                Code::Io.as_str(),
                format!("could not be removed ({error}); remove it by hand"),
            ));
            return Err(error);
        }
        let Some(directory) = directories.get(parent, None)? else {
            return Err(AccessError::Unexpected(parent.to_owned()));
        };
        directory.remove_file(name)?;
    }
    published.push(file);
    directories.changed.insert(parent.to_owned());
    Ok(())
}

/// Removes the staging files of `staged`.
fn discard(directories: &mut Directories<'_>, staged: &[&str]) {
    for path in staged {
        directories.clean(&staging_path(path), false);
    }
}

/// Restores published files from their text before, each with the access
/// it had; returns the paths that could not be restored.
fn roll_back<'f>(
    directories: &mut Directories<'_>,
    known: &Known,
    published: &[&'f FileChange],
) -> Vec<&'f str> {
    let mut unrestored = Vec::new();
    for file in published.iter().rev() {
        let (parent, name) = split(&file.path);
        let restored = match &file.before {
            Some(text) => {
                let access = known.files.get(&file.path).copied().flatten();
                directories
                    .get(parent, known.new_directory(&file.path))
                    .and_then(|directory| {
                        let directory =
                            directory.ok_or_else(|| AccessError::Unexpected(parent.to_owned()))?;
                        directory.write(&staging_name(name), access.as_ref(), text.as_bytes())?;
                        // Restored only with no access beyond its mode bits.
                        directory.access(&staging_name(name))?;
                        directory.rename(&staging_name(name), name)
                    })
            }
            None => directories.get(parent, None).and_then(|directory| {
                directory
                    .ok_or_else(|| AccessError::Unexpected(parent.to_owned()))?
                    .remove_file(name)
                    .map(|_| ())
            }),
        };
        directories.changed.insert(parent.to_owned());
        if restored.is_err() {
            unrestored.push(file.path.as_str());
            // A staging file the failed restore left goes, or is reported.
            directories.clean(&staging_path(&file.path), false);
        }
    }
    unrestored
}

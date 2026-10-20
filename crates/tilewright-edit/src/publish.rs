// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Writing a plan: under the root's lock, stage every new file beside its
//! target, check the files and validate the result again, then rename the
//! staged files into place and remove deleted ones. A failure while
//! publishing restores the files already published from the plan's copies.

use crate::{
    Code, FileChange, Plan, diagnostic, digest, lineage::Lineage, staging_name, staging_path,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
use tilewright_source::{AccessError, Diagnostic, SourceDirectory, SourceRoot};

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
    /// The permissions each changed file had before publishing, to
    /// restore it with.
    original: BTreeMap<String, std::fs::Permissions>,
    /// Cleanup that could not be done, and how many more past the limit.
    leftovers: Vec<Diagnostic>,
    dropped: usize,
}

impl Directories<'_> {
    /// The directory `path`, created with its missing parents if `create`.
    fn get(&mut self, path: &str, create: bool) -> Result<Option<&SourceDirectory>, AccessError> {
        if !self.open.contains_key(path) {
            if create {
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
        let removed = match self.get(parent, false)? {
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
        match self.get(path, false) {
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

/// The permissions found so far: of each path on disk, and of each step of
/// the lineage, so that each is looked up once per publish.
#[derive(Default)]
struct Known {
    paths: BTreeMap<String, Option<std::fs::Permissions>>,
    steps: BTreeMap<usize, Option<std::fs::Permissions>>,
}

/// The permissions of the file at `path` on disk, if there is one.
fn on_disk(
    directories: &mut Directories<'_>,
    known: &mut Known,
    path: &str,
) -> Result<Option<std::fs::Permissions>, AccessError> {
    if let Some(found) = known.paths.get(path) {
        return Ok(found.clone());
    }
    let (parent, name) = split(path);
    let found = match directories.get(parent, false)? {
        Some(directory) => directory.permissions(name)?,
        None => None,
    };
    known.paths.insert(path.to_owned(), found.clone());
    Ok(found)
}

/// Checks that the permissions staging read are still those on disk, so
/// that no file is published with permissions that were narrowed since.
fn check_permissions(
    directories: &mut Directories<'_>,
    known: &Known,
) -> Result<(), Vec<Diagnostic>> {
    for (path, before) in &known.paths {
        let (parent, name) = split(path);
        let now = match directories.get(parent, false) {
            Ok(Some(directory)) => directory.permissions(name),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        }
        .map_err(|error| io(path, &error))?;
        if now != *before {
            return Err(problem(
                path,
                Code::Conflict,
                "its permissions changed while the transaction was being applied; nothing was written",
            ));
        }
    }
    Ok(())
}

/// The permissions of the file at `path` on disk, noted to restore it with
/// if publishing fails.
fn original(
    directories: &mut Directories<'_>,
    known: &mut Known,
    path: &str,
) -> Result<Option<std::fs::Permissions>, AccessError> {
    let existing = on_disk(directories, known, path)?;
    if let Some(existing) = &existing {
        directories
            .original
            .insert(path.to_owned(), existing.clone());
    }
    Ok(existing)
}

/// What both allow, where either may be unknown.
fn both(
    left: Option<std::fs::Permissions>,
    right: Option<std::fs::Permissions>,
) -> Option<std::fs::Permissions> {
    match (left, right) {
        (Some(left), Some(right)) => Some(narrowest(&left, &right)),
        (left, right) => left.or(right),
    }
}

/// The permissions a written file gets, and keeps a note of the ones it had
/// for rollback. A file written where a file is keeps what that file
/// allows. One renamed or created like another is also restricted to what
/// each file of its chain allows on disk, so that access is never broader
/// than any of theirs. With none of these on disk, a new file gets the
/// platform's default (`None`).
fn permissions_for(
    directories: &mut Directories<'_>,
    known: &mut Known,
    path: &str,
    lineage: &Lineage,
) -> Result<Option<std::fs::Permissions>, AccessError> {
    let existing = original(directories, known, path)?;
    // Walk back to a step already known, then work forward: each step
    // allows what its file on disk and the steps before it allow.
    let mut pending = Vec::new();
    let mut next = lineage.step(path);
    let mut inherited = None;
    while let Some(step) = next {
        if let Some(found) = known.steps.get(&step) {
            inherited.clone_from(found);
            break;
        }
        pending.push(step);
        next = lineage.at(step).1;
    }
    for step in pending.into_iter().rev() {
        let own = on_disk(directories, known, lineage.at(step).0)?;
        inherited = both(own, inherited);
        known.steps.insert(step, inherited.clone());
    }
    Ok(both(existing, inherited))
}

/// Permissions that allow only what both allow.
fn narrowest(left: &std::fs::Permissions, right: &std::fs::Permissions) -> std::fs::Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::Permissions::from_mode(left.mode() & right.mode())
    }
    #[cfg(not(unix))]
    {
        let mut narrowest = left.clone();
        narrowest.set_readonly(left.readonly() || right.readonly());
        narrowest
    }
}

/// Writes `text` to the staging file of `path` in `directory`.
fn stage(directory: &SourceDirectory, path: &str, text: &str) -> Result<(), AccessError> {
    let name = split(path).1;
    directory.write(&staging_name(name), name, text.as_bytes())
}

/// Stages every new file. On failure removes every staging file it wrote,
/// the failed one included, and the directories it created.
fn stage_all<'f>(
    directories: &mut Directories<'_>,
    known: &mut Known,
    changed: &[&'f FileChange],
    lineage: &Lineage,
) -> Result<Vec<&'f str>, PublishError> {
    let mut staged = Vec::new();
    for file in changed {
        let result = match &file.after {
            // A deleted file's permissions are read now, and checked again
            // with the others, to restore it with if publishing fails.
            None => original(directories, known, &file.path).map(|_| ()),
            Some(text) => {
                staged.push(file.path.as_str());
                // Writing removes whatever was at the staging name first,
                // so the directory may change even if the write fails.
                directories.changed.insert(split(&file.path).0.to_owned());
                let (parent, name) = split(&file.path);
                permissions_for(directories, known, &file.path, lineage).and_then(|permissions| {
                    let directory = directories
                        .get(parent, true)?
                        .ok_or_else(|| AccessError::Unexpected(file.path.clone()))?;
                    directory.write_with(&staging_name(name), permissions.as_ref(), text.as_bytes())
                })
            }
        };
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
            // A file grown past what can be read changed too.
            Err(AccessError::TooLarge(_)) => return Err(changed()),
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
            original: BTreeMap::new(),
            leftovers: Vec::new(),
            dropped: 0,
        };
        // Files are written first and removed last, so that whatever stops
        // a publish halfway, a renamed file is never only removed.
        let mut changed: Vec<&FileChange> =
            self.files.iter().filter(|file| file.is_changed()).collect();
        changed.sort_by_key(|file| file.after.is_none());
        let mut known = Known::default();
        let staged = stage_all(&mut directories, &mut known, &changed, &self.lineage)?;

        #[cfg(test)]
        if let Some(hook) = BEFORE_CHECK.with(|hook| hook.borrow_mut().take()) {
            hook();
        }

        let checked = check_again(root, &self.files).and_then(|()| {
            let world = tilewright_source::load_edited_in(root, &self.load, &self.overlay())?;
            let problems: Vec<Diagnostic> = self
                .terrain
                .iter()
                .filter_map(|(plane, grid, at)| {
                    crate::terrain_problem(&world, plane, grid).map(|message| Diagnostic {
                        message,
                        ..at.clone()
                    })
                })
                .take(crate::limits::DIAGNOSTICS)
                .collect();
            if problems.is_empty() {
                Ok(())
            } else {
                Err(problems)
            }
        });
        // Last, as close to publishing as can be: the permissions the
        // staged files were given must still be what they came from.
        let checked = checked.and_then(|()| check_permissions(&mut directories, &known));
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
                let unrestored = roll_back(&mut directories, &published);
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
        let Some(directory) = directories.get(parent, false)? else {
            return Err(AccessError::Unexpected(parent.to_owned()));
        };
        directory.rename(&staging_name(name), name)?;
    } else {
        // A staging file left by an interrupted apply goes first, so that
        // failing to remove it leaves the target untouched; its removal is
        // recorded to sync whatever happens next. The target's permissions
        // were noted, and checked, before publishing began.
        if let Err(error) = directories.remove(&staging_path(&file.path), false) {
            directories.leftover(diagnostic::file(
                &staging_path(&file.path),
                Code::Io.as_str(),
                format!("could not be removed ({error}); remove it by hand"),
            ));
            return Err(error);
        }
        let Some(directory) = directories.get(parent, false)? else {
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

/// Restores published files from their text before; returns the paths
/// that could not be restored.
fn roll_back<'f>(directories: &mut Directories<'_>, published: &[&'f FileChange]) -> Vec<&'f str> {
    let mut unrestored = Vec::new();
    for file in published.iter().rev() {
        let (parent, name) = split(&file.path);
        let restored = match &file.before {
            Some(text) => {
                // A file comes back with the permissions it had.
                let permissions = directories.original.get(&file.path).cloned();
                directories.get(parent, true).and_then(|directory| {
                    let directory =
                        directory.ok_or_else(|| AccessError::Unexpected(parent.to_owned()))?;
                    match permissions {
                        Some(permissions) => directory.write_with(
                            &staging_name(name),
                            Some(&permissions),
                            text.as_bytes(),
                        )?,
                        None => stage(directory, &file.path, text)?,
                    }
                    directory.rename(&staging_name(name), name)
                })
            }
            None => directories.get(parent, false).and_then(|directory| {
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

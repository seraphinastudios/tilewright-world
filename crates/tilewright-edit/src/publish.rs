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
    /// Whether every file was published. When it was, only making the
    /// result durable, or removing a stale staging file beside it, failed:
    /// the tree holds the transaction's result.
    /// When it was not, the tree holds the files as they were, except any
    /// that the problems name as not restored.
    pub applied: bool,
    pub problems: Vec<Diagnostic>,
}

impl fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = if self.applied {
            "applied, but not made durable"
        } else {
            "not applied"
        };
        write!(formatter, "the transaction was {state}")?;
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
        problems,
    }
}

/// The directories of a publish, opened once each.
struct Directories<'r> {
    root: &'r SourceRoot,
    open: BTreeMap<String, SourceDirectory>,
    /// Directories this publish created, in creation order.
    created: Vec<String>,
    /// Directories whose entries changed, to sync.
    changed: BTreeSet<String>,
    /// The permissions of each deleted file, to restore it with.
    deleted: BTreeMap<String, std::fs::Permissions>,
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
                    if self.root.directory(&prefix, false)?.is_none() {
                        self.root.directory(&prefix, true)?;
                        self.created.push(prefix.clone());
                        self.changed.insert(split(&prefix).0.to_owned());
                    }
                }
            }
            match self.root.directory(path, false)? {
                Some(directory) => {
                    self.open.insert(path.to_owned(), directory);
                }
                None => return Ok(None),
            }
        }
        Ok(self.open.get(path))
    }

    /// Removes the directories this publish created, if they are empty.
    fn remove_created(&mut self) {
        for path in std::mem::take(&mut self.created).into_iter().rev() {
            self.open.remove(&path);
            self.changed.remove(&path);
            let (parent, name) = split(&path);
            if let Ok(Some(directory)) = self.root.directory(parent, false)
                && directory.remove_directory(name).unwrap_or(false)
            {
                self.changed.insert(parent.to_owned());
            }
        }
    }

    /// Removes the directories above a deleted file that are now empty,
    /// up to the top-level directories. Returns what could not be done; a
    /// directory that is not empty is left, as it should be.
    fn prune(&mut self, path: &str) -> Vec<Diagnostic> {
        let mut current = split(path).0.to_owned();
        while current.contains('/') {
            let (parent, name) = split(&current);
            let removed =
                self.root
                    .directory(parent, false)
                    .and_then(|directory| match directory {
                        Some(directory) => directory.remove_directory(name),
                        None => Ok(false),
                    });
            match removed {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => return io(&current, &error),
            }
            // Only a directory that is gone leaves the set to sync.
            self.open.remove(&current);
            self.changed.remove(&current);
            self.changed.insert(parent.to_owned());
            current = parent.to_owned();
        }
        Vec::new()
    }

    /// Syncs every directory whose entries changed. Returns the failures,
    /// and whether the platform could sync every one.
    fn sync(&mut self) -> (Vec<Diagnostic>, bool) {
        let mut failures = Vec::new();
        let mut durable = true;
        for path in std::mem::take(&mut self.changed) {
            #[cfg(test)]
            if FAIL_SYNC.with(std::cell::Cell::get) {
                failures.extend(problem(&path, Code::Io, "injected sync failure"));
                continue;
            }
            match self.get(&path, false) {
                Ok(Some(directory)) => match directory.sync() {
                    Ok(synced) => durable &= synced,
                    Err(error) => failures.extend(io(&path, &error)),
                },
                Ok(None) => {}
                Err(error) => failures.extend(io(&path, &error)),
            }
        }
        (failures, durable)
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
    changed: &[&'f FileChange],
) -> Result<Vec<&'f str>, PublishError> {
    let mut staged = Vec::new();
    for file in changed {
        let Some(text) = &file.after else {
            continue;
        };
        staged.push(file.path.as_str());
        let result =
            directories
                .get(split(&file.path).0, true)
                .and_then(|directory| match directory {
                    Some(directory) => stage(directory, &file.path, text),
                    None => Err(AccessError::Unexpected(file.path.clone())),
                });
        if let Err(error) = result {
            discard(directories, &staged);
            directories.remove_created();
            let mut problems = io(&file.path, &error);
            problems.extend(directories.sync().0);
            return Err(not_applied(problems));
        }
    }
    Ok(staged)
}

/// Checks that every file is as it was planned.
fn check_again(root: &SourceRoot, files: &[FileChange]) -> Result<(), Vec<Diagnostic>> {
    let limit =
        u64::try_from(tilewright_jsonc::Limits::default().max_input_bytes).unwrap_or(u64::MAX);
    for file in files {
        let current = root
            .read(&file.path, limit)
            .map_err(|error| io(&file.path, &error))?;
        let found = current.map(|bytes| {
            use sha2::Digest as _;
            <[u8; 32]>::from(sha2::Sha256::digest(&bytes))
        });
        if found != digest(file.before.as_deref()) {
            return Err(problem(
                &file.path,
                Code::Conflict,
                "the file changed while the transaction was being applied; nothing was written",
            ));
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
    /// A failure to sync after every file was published is reported with
    /// `applied` set.
    pub fn publish(&self) -> Result<Published, PublishError> {
        let root = SourceRoot::new(&self.root);
        let _lock = root.lock().map_err(|error| {
            not_applied(match error {
                AccessError::Locked => problem(".", Code::Conflict, error.to_string()),
                other => io(".", &other),
            })
        })?;
        let mut directories = Directories {
            root: &root,
            open: BTreeMap::new(),
            created: Vec::new(),
            changed: BTreeSet::new(),
            deleted: BTreeMap::new(),
        };
        let changed: Vec<&FileChange> =
            self.files.iter().filter(|file| file.is_changed()).collect();
        let staged = stage_all(&mut directories, &changed)?;

        #[cfg(test)]
        if let Some(hook) = BEFORE_CHECK.with(|hook| hook.borrow_mut().take()) {
            hook();
        }

        let checked = check_again(&root, &self.files).and_then(|()| {
            tilewright_source::load_edited(&self.root, &self.load, &self.overlay()).map(|_| ())
        });
        if let Err(mut problems) = checked {
            discard(&mut directories, &staged);
            directories.remove_created();
            problems.extend(directories.sync().0);
            return Err(not_applied(problems));
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
                let mut unrestored = roll_back(&mut directories, &published);
                discard(&mut directories, &staged);
                directories.remove_created();
                unrestored.extend(
                    directories
                        .sync()
                        .0
                        .into_iter()
                        .map(|failure| failure.to_string()),
                );
                let message = if unrestored.is_empty() {
                    format!("{error}; every file published before it was restored")
                } else {
                    format!(
                        "{error}; restoring failed for: {}; apply the inverse to restore the rest",
                        unrestored.join(", ")
                    )
                };
                return Err(not_applied(problem(&file.path, Code::Io, message)));
            }
        }
        // Staging files left beside the files of this plan by an earlier,
        // interrupted apply go too; one that cannot go is reported, since
        // the tree does not validate while it is there.
        let mut failures = Vec::new();
        for file in &self.files {
            let (parent, name) = split(&file.path);
            let removed = directories
                .get(parent, false)
                .and_then(|directory| match directory {
                    Some(directory) => directory.remove_file(&staging_name(name)),
                    None => Ok(false),
                });
            match removed {
                Ok(true) => {
                    directories.changed.insert(parent.to_owned());
                }
                Ok(false) => {}
                Err(error) => failures.extend(io(&staging_path(&file.path), &error)),
            }
        }
        for file in &changed {
            if file.after.is_none() {
                failures.extend(directories.prune(&file.path));
            }
        }
        let (synced, durable) = directories.sync();
        failures.extend(synced);
        if failures.is_empty() {
            Ok(Published { durable })
        } else {
            Err(PublishError {
                applied: true,
                problems: failures,
            })
        }
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
    let Some(directory) = directories.get(parent, false)? else {
        return Err(AccessError::Unexpected(parent.to_owned()));
    };
    if write {
        directory.rename(&staging_name(name), name)?;
    } else {
        // A staging file left by an interrupted apply goes first, so that
        // failing to remove it leaves the target untouched; the target's
        // permissions are kept to restore it with.
        directory.remove_file(&staging_name(name))?;
        let permissions = directory.permissions(name)?;
        directory.remove_file(name)?;
        if let Some(permissions) = permissions {
            directories.deleted.insert(file.path.clone(), permissions);
        }
    }
    published.push(file);
    directories.changed.insert(parent.to_owned());
    Ok(())
}

/// Removes the staging files of `staged`.
fn discard(directories: &mut Directories<'_>, staged: &[&str]) {
    for path in staged {
        let (parent, name) = split(path);
        if let Ok(Some(directory)) = directories.get(parent, false)
            && directory.remove_file(&staging_name(name)).unwrap_or(false)
        {
            directories.changed.insert(parent.to_owned());
        }
    }
}

/// Restores published files from their text before; returns the paths
/// that could not be restored.
fn roll_back(directories: &mut Directories<'_>, published: &[&FileChange]) -> Vec<String> {
    let mut unrestored = Vec::new();
    for file in published.iter().rev() {
        let (parent, name) = split(&file.path);
        let restored = match &file.before {
            Some(text) => {
                // A deleted file comes back with its own permissions; a
                // replaced one keeps those of the file now in its place.
                let permissions = directories.deleted.get(&file.path).cloned();
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
            unrestored.push(file.path.clone());
        }
    }
    unrestored
}

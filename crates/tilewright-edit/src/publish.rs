// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Writing a plan: stage every new file beside its target, check the base
//! again under the root's lock, then rename the staged files into place and
//! remove deleted ones. A failure while publishing restores the files
//! already published from the plan's copies.

use crate::{Code, FileChange, Plan, diagnostic, digest, staging_name};
use std::collections::BTreeMap;
use tilewright_source::{AccessError, Diagnostic, SourceDirectory, SourceRoot};

#[cfg(test)]
thread_local! {
    /// Fails the publish step with this index, for tests.
    pub(crate) static FAIL_STEP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Runs after staging and before the base is checked again, for tests.
    pub(crate) static BEFORE_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
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

/// The directories of a publish, opened once each.
struct Directories<'r> {
    root: &'r SourceRoot,
    open: BTreeMap<String, SourceDirectory>,
    /// Directories this publish created, in creation order.
    created: Vec<String>,
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
        for path in self.created.drain(..).rev() {
            self.open.remove(&path);
            let (parent, name) = split(&path);
            if let Ok(Some(parent)) = self.root.directory(parent, false) {
                let _ = parent.remove_directory(name);
            }
        }
    }

    /// Removes the directories above a deleted file that are now empty,
    /// up to the top-level directories.
    fn prune(&mut self, path: &str) {
        let mut current = split(path).0.to_owned();
        while current.contains('/') {
            self.open.remove(&current);
            let (parent, name) = split(&current);
            let removed = matches!(
                self.root.directory(parent, false),
                Ok(Some(directory)) if directory.remove_directory(name).unwrap_or(false)
            );
            if !removed {
                break;
            }
            current = parent.to_owned();
        }
    }
}

/// Writes `text` to the staging file of `path` in `directory`.
fn stage(directory: &SourceDirectory, path: &str, text: &str) -> Result<(), AccessError> {
    let name = split(path).1;
    directory.write(&staging_name(name), name, text.as_bytes())
}

/// Stages every new file; on failure removes what was staged.
fn stage_all<'f>(
    directories: &mut Directories<'_>,
    changed: &[&'f FileChange],
) -> Result<Vec<&'f str>, Vec<Diagnostic>> {
    let mut staged = Vec::new();
    for file in changed {
        let Some(text) = &file.after else {
            continue;
        };
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
            return Err(io(&file.path, &error));
        }
        staged.push(file.path.as_str());
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

/// Syncs every directory the publish changed, the parents of the ones it
/// created included, and removes those deleting files emptied.
fn finish(
    directories: &mut Directories<'_>,
    changed: &[&FileChange],
) -> Result<(), Vec<Diagnostic>> {
    let mut failures = Vec::new();
    let parents: Vec<String> = directories
        .created
        .iter()
        .map(|path| split(path).0.to_owned())
        .collect();
    for parent in parents {
        if let Err(error) = directories.get(&parent, false) {
            failures.extend(io(&parent, &error));
        }
    }
    for (path, directory) in &directories.open {
        if let Err(error) = directory.sync() {
            failures.extend(io(path, &error));
        }
    }
    for file in changed {
        if file.after.is_none() {
            directories.prune(&file.path);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

impl Plan {
    /// Writes the plan's files. The root's lock is held throughout; every
    /// file the plan touched or consulted must still be as it was planned.
    ///
    /// # Errors
    /// Returns a `tx.conflict` diagnostic, with nothing written, when a
    /// file changed since planning or another edit holds the lock, and
    /// `tx.io` or `tx.path` ones when writing fails. A failure after files
    /// were published restores them; the diagnostic names any that could
    /// not be restored.
    pub fn publish(&self) -> Result<(), Vec<Diagnostic>> {
        let root = SourceRoot::new(&self.root);
        let _lock = root.lock().map_err(|error| match error {
            AccessError::Locked => problem(".", Code::Conflict, error.to_string()),
            other => io(".", &other),
        })?;
        let mut directories = Directories {
            root: &root,
            open: BTreeMap::new(),
            created: Vec::new(),
        };
        let changed: Vec<&FileChange> =
            self.files.iter().filter(|file| file.is_changed()).collect();
        let staged = stage_all(&mut directories, &changed)?;

        #[cfg(test)]
        if let Some(hook) = BEFORE_CHECK.with(|hook| hook.borrow_mut().take()) {
            hook();
        }

        if let Err(problems) = check_again(&root, &self.files) {
            discard(&mut directories, &staged);
            directories.remove_created();
            return Err(problems);
        }
        let mut published: Vec<&FileChange> = Vec::new();
        for (step, file) in changed.iter().enumerate() {
            let (parent, name) = split(&file.path);
            let result = publish_one(&mut directories, parent, name, file.after.is_some(), step);
            if let Err(error) = result {
                let unrestored = roll_back(&mut directories, &published);
                discard(&mut directories, &staged);
                directories.remove_created();
                let message = if unrestored.is_empty() {
                    format!("{error}; every file published before it was restored")
                } else {
                    format!(
                        "{error}; these files could not be restored: {}; apply the inverse to restore them",
                        unrestored.join(", ")
                    )
                };
                return Err(problem(&file.path, Code::Io, message));
            }
            published.push(file);
        }
        finish(&mut directories, &changed)
    }
}

fn publish_one(
    directories: &mut Directories<'_>,
    parent: &str,
    name: &str,
    write: bool,
    step: usize,
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
        directory.rename(&staging_name(name), name)
    } else {
        directory.remove_file(name)?;
        // A staging file left by an interrupted apply goes too.
        directory.remove_file(&staging_name(name)).map(|_| ())
    }
}

/// Removes the staging files of `staged`.
fn discard(directories: &mut Directories<'_>, staged: &[&str]) {
    for path in staged {
        let (parent, name) = split(path);
        if let Ok(Some(directory)) = directories.get(parent, false) {
            let _ = directory.remove_file(&staging_name(name));
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
            Some(text) => directories.get(parent, true).and_then(|directory| {
                let directory =
                    directory.ok_or_else(|| AccessError::Unexpected(parent.to_owned()))?;
                stage(directory, &file.path, text)?;
                directory.rename(&staging_name(name), name)
            }),
            None => directories.get(parent, false).and_then(|directory| {
                directory
                    .ok_or_else(|| AccessError::Unexpected(parent.to_owned()))?
                    .remove_file(name)
                    .map(|_| ())
            }),
        };
        if restored.is_err() {
            unrestored.push(file.path.clone());
        }
    }
    unrestored
}

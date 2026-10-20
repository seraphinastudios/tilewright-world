// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Uid addresses: where each object with a uid is, in the world as loaded
//! before the transaction and as the operations so far have left it.

use crate::{
    diagnostic,
    engine::{Failure, Outcome, Workspace},
    operations::chunk_site,
    splice::Step,
};
use std::collections::{BTreeMap, BTreeSet};
use tilewright_jsonc::Value;
use tilewright_source::{LoadOptions, Overlay, SourceRoot};

/// Where the objects with uids are, by chunk path.
pub(crate) struct Index {
    /// The chunk of each uid of the world as loaded, or why the tree does
    /// not load.
    loaded: Result<BTreeMap<String, String>, String>,
    /// The uids each chunk the transaction changed holds now.
    files: BTreeMap<String, Vec<String>>,
    /// The changed chunks that hold each of those uids now.
    holders: BTreeMap<String, BTreeSet<String>>,
}

impl Index {
    /// Loads the tree at `root` for the chunk of every uid.
    pub fn load(root: &SourceRoot, options: &LoadOptions) -> Self {
        let loaded = match tilewright_source::load_edited_in(root, options, &Overlay::new()) {
            Ok(world) => Ok(world
                .object_uids()
                .iter()
                .filter_map(|(uid, location)| {
                    let plane = world.planes().get(&location.plane)?;
                    let chunk = plane.chunks.get(&location.chunk)?;
                    Some((uid.clone(), chunk.path.clone()))
                })
                .collect()),
            Err(problems) => Err(problems.first().map_or_else(
                || "the tree does not load".to_owned(),
                |problem| {
                    let more = match problems.len() {
                        1 => String::new(),
                        count => format!(", and {} more problem(s)", count - 1),
                    };
                    format!(
                        "the tree does not load ({}: {}{more})",
                        problem.path, problem.message
                    )
                },
            )),
        };
        Self {
            loaded,
            files: BTreeMap::new(),
            holders: BTreeMap::new(),
        }
    }

    /// Indexes again the chunks changed since the last lookup. A chunk
    /// that cannot be read stays to be indexed.
    fn refresh(&mut self, workspace: &mut Workspace<'_>) -> Outcome<()> {
        while let Some(path) = workspace.changed.pop_first() {
            if chunk_site(&path).is_none() {
                continue;
            }
            let held = match held(workspace, &path) {
                Ok(held) => held,
                Err(failure) => {
                    workspace.changed.insert(path);
                    return Err(failure);
                }
            };
            for uid in self.files.remove(&path).unwrap_or_default() {
                if let Some(holders) = self.holders.get_mut(&uid) {
                    holders.remove(&path);
                    if holders.is_empty() {
                        self.holders.remove(&uid);
                    }
                }
            }
            for uid in &held {
                self.holders
                    .entry(uid.clone())
                    .or_default()
                    .insert(path.clone());
            }
            self.files.insert(path, held);
        }
        Ok(())
    }

    /// The chunks that hold `uid` now: those the transaction changed that
    /// hold it, and the one the world as loaded has it in if unchanged.
    fn chunks(&self, uid: &str) -> Outcome<BTreeSet<String>> {
        let loaded = self.loaded.as_ref().map_err(|reason| {
            Failure::conflict(format!(
                "uid {} cannot be resolved: {reason}",
                diagnostic::quoted(uid)
            ))
        })?;
        let mut chunks = self.holders.get(uid).cloned().unwrap_or_default();
        if let Some(path) = loaded.get(uid)
            && !self.files.contains_key(path)
        {
            chunks.insert(path.clone());
        }
        Ok(chunks)
    }
}

/// The uids of the chunk at `path` as it is now, each once.
fn held(workspace: &mut Workspace<'_>, path: &str) -> Outcome<Vec<String>> {
    if !workspace.exists(path)? {
        return Ok(Vec::new());
    }
    let mut uids = BTreeSet::new();
    walk(workspace.document(path)?.root(), |_, uid| {
        uids.insert(uid.to_owned());
    });
    Ok(uids.into_iter().collect())
}

/// Calls `visit` with the pointer and uid of every object of a chunk's
/// `objects` that has a uid, inventory entries included, in depth-first
/// order. Documents nest at most as deep as the JSONC limits allow.
fn walk(root: &Value, mut visit: impl FnMut(&[Step], &str)) {
    let Some(objects) = root.get("objects").and_then(Value::as_array) else {
        return;
    };
    let mut pending: Vec<(Vec<Step>, &Value)> = objects
        .iter()
        .enumerate()
        .rev()
        .map(|(position, object)| {
            (
                vec![Step::Key("objects".to_owned()), Step::Index(position)],
                object,
            )
        })
        .collect();
    while let Some((pointer, object)) = pending.pop() {
        if let Some(uid) = object.get("uid").and_then(Value::as_str) {
            visit(&pointer, uid);
        }
        if let Some(inventory) = object.get("inventory").and_then(Value::as_array) {
            for (position, item) in inventory.iter().enumerate().rev() {
                let mut inner = pointer.clone();
                inner.push(Step::Key("inventory".to_owned()));
                inner.push(Step::Index(position));
                pending.push((inner, item));
            }
        }
    }
}

fn listed(paths: &BTreeSet<String>) -> String {
    diagnostic::listed(paths.iter().map(String::as_str))
}

impl Workspace<'_> {
    /// The chunk that holds the object with `uid` now, and the steps to
    /// the object in it. A uid that no object has, or more than one, is a
    /// conflict.
    pub fn find_uid(&mut self, uid: &str) -> Outcome<(String, Vec<Step>)> {
        let Some(mut index) = self.uids.take() else {
            return Err(Failure::conflict(format!(
                "uid {} cannot be resolved",
                diagnostic::quoted(uid)
            )));
        };
        let chunks = index.refresh(self).and_then(|()| index.chunks(uid));
        self.uids = Some(index);
        let chunks = chunks?;
        let missing =
            || Failure::conflict(format!("no object has uid {}", diagnostic::quoted(uid)));
        let twice = |paths: &BTreeSet<String>| {
            Failure::conflict(format!(
                "more than one object has uid {}, in {}",
                diagnostic::quoted(uid),
                listed(paths)
            ))
        };
        let mut paths = chunks.iter();
        let (Some(path), None) = (paths.next(), paths.next()) else {
            return Err(if chunks.is_empty() {
                missing()
            } else {
                twice(&chunks)
            });
        };
        if !self.exists(path)? {
            return Err(missing());
        }
        let mut found = Vec::new();
        walk(self.document(path)?.root(), |pointer, held| {
            if held == uid {
                found.push(pointer.to_vec());
            }
        });
        match found.len() {
            0 => Err(missing()),
            1 => Ok((path.clone(), found.swap_remove(0))),
            _ => Err(twice(&chunks)),
        }
    }
}

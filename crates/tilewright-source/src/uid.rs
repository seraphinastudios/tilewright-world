// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Object uids: the claims a load makes as it reads chunks, so a duplicate
//! is reported where it is read, and the index of a loaded world.

use crate::{Object, ObjectLocation, Plane};
use std::{collections::BTreeMap, sync::Arc};

/// The uids claimed so far in one load, each with the path of the chunk
/// whose object holds it. Uids are unique in the whole world, so one set
/// spans every plane.
#[derive(Default)]
pub(crate) struct Claims(BTreeMap<String, Arc<str>>);

impl Claims {
    /// Claims `uid` for an object of the chunk at `path`. Returns the path
    /// of the chunk whose object already holds it, if one does.
    pub fn claim(&mut self, uid: &str, path: &Arc<str>) -> Option<Arc<str>> {
        if let Some(first) = self.0.get(uid) {
            return Some(Arc::clone(first));
        }
        self.0.insert(uid.to_owned(), Arc::clone(path));
        None
    }
}

/// Where every object with a uid is. Of objects that share a uid, which a
/// valid world never has, the first in plane, chunk and object order wins.
pub(crate) fn index(planes: &BTreeMap<String, Plane>) -> BTreeMap<String, ObjectLocation> {
    let mut index = BTreeMap::new();
    let mut path = Vec::new();
    for (id, plane) in planes {
        for (key, chunk) in &plane.chunks {
            for (position, object) in chunk.objects.iter().enumerate() {
                path.push(position);
                add(&mut index, id, *key, object, &mut path);
                path.pop();
            }
        }
    }
    index
}

/// Adds `object`, at `path`, and its inventory to `index`. Inventories
/// nest at most 16 deep, which bounds the recursion.
fn add(
    index: &mut BTreeMap<String, ObjectLocation>,
    plane: &str,
    chunk: (i32, i64, i64),
    object: &Object,
    path: &mut Vec<usize>,
) {
    if let Some(uid) = &object.uid {
        index.entry(uid.clone()).or_insert_with(|| ObjectLocation {
            plane: plane.to_owned(),
            chunk,
            path: path.clone(),
        });
    }
    for (position, item) in object.inventory.iter().enumerate() {
        path.push(position);
        add(index, plane, chunk, item, path);
        path.pop();
    }
}

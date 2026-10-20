// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Comparing two snapshots: removed, added and renamed IDs, and which
//! removals an allowlist accepts (`docs/spec/DIFF.md`).

use crate::{
    Added, Allowance, Allowlist, Basis, Kind, Reason, Removed, Renamed, Report, Snapshot, Unused,
    allow::Intent, snapshot::Fingerprint,
};
use std::collections::BTreeMap;

/// The order kinds are paired in: planes first, so that zones, anchors and
/// stairwells of a renamed plane pair with those of its new ID.
const PAIRING_ORDER: [Kind; 7] = [
    Kind::Plane,
    Kind::Anchor,
    Kind::Definition,
    Kind::Object,
    Kind::RenderLayer,
    Kind::Stairwell,
    Kind::Zone,
];

/// Removed and added IDs, grouped by scope and content.
type Groups<'a> = BTreeMap<(&'a str, &'a Fingerprint), (Vec<&'a str>, Vec<&'a str>)>;

/// Compares two snapshots. Every list in the report is sorted, so the same
/// snapshots and allowlist always give the same report.
#[must_use]
pub fn diff(old: &Snapshot, new: &Snapshot, allowlist: &Allowlist) -> Report {
    let mut removed = Vec::new();
    let mut renamed = Vec::new();
    let mut added = Vec::new();
    // Each renamed plane's new ID, by its old ID.
    let mut planes: BTreeMap<&str, &str> = BTreeMap::new();
    for kind in PAIRING_ORDER {
        let mut gone: BTreeMap<&str, &Fingerprint> = old
            .entries(kind)
            .filter(|(id, _)| !new.contains(kind, id))
            .collect();
        let mut fresh: BTreeMap<&str, &Fingerprint> = new
            .entries(kind)
            .filter(|(id, _)| !old.contains(kind, id))
            .collect();
        let mut pairs = Vec::new();
        // A declared rename pairs its IDs whenever the new one exists.
        for from in gone.keys() {
            if let Some(Intent::Rename(to)) = allowlist.get(kind, from)
                && new.contains(kind, to)
            {
                pairs.push((*from, to, Basis::Declared));
            }
        }
        for (from, to, _) in &pairs {
            gone.remove(from);
            fresh.remove(to);
        }
        // Otherwise, a removed and an added ID pair when they are the only
        // two with the same scope and content, for kinds that pair so.
        if kind.pairs_by_content() {
            for (from, to) in content_pairs(kind, &gone, &fresh, &planes) {
                gone.remove(from);
                fresh.remove(to);
                pairs.push((from, to, Basis::Content));
            }
        }
        if kind == Kind::Plane {
            planes = pairs.iter().map(|(from, to, _)| (*from, *to)).collect();
        }
        removed.extend(gone.into_keys().map(|id| Removed {
            kind,
            id: id.to_owned(),
            allowed: allowed(kind, id, allowlist, old, new),
        }));
        renamed.extend(pairs.into_iter().map(|(from, to, basis)| Renamed {
            kind,
            from: from.to_owned(),
            to: to.to_owned(),
            basis,
            allowed: allowed(kind, from, allowlist, old, new),
        }));
        added.extend(fresh.into_keys().map(|id| Added {
            kind,
            id: id.to_owned(),
        }));
    }
    removed.sort();
    renamed.sort();
    added.sort();
    let unused = allowlist
        .allowances()
        .filter_map(|allowance| {
            let kind = allowance.kind();
            let reason =
                if !old.contains(kind, allowance.id()) || new.contains(kind, allowance.id()) {
                    Reason::NotRemoved
                } else {
                    match &allowance {
                        Allowance::Rename { to, .. } if !new.contains(kind, to) => {
                            Reason::TargetMissing
                        }
                        _ => return None,
                    }
                };
            Some(Unused { allowance, reason })
        })
        .collect();
    Report {
        old: old.side().clone(),
        new: new.side().clone(),
        removed,
        renamed,
        added,
        unused,
    }
}

/// The removed and added IDs of `kind` that are the only two with their
/// scope and content. `planes` maps renamed planes to their new IDs, so the
/// IDs of a renamed plane are in its new plane.
fn content_pairs<'a>(
    kind: Kind,
    gone: &BTreeMap<&'a str, &'a Fingerprint>,
    fresh: &BTreeMap<&'a str, &'a Fingerprint>,
    planes: &BTreeMap<&'a str, &'a str>,
) -> Vec<(&'a str, &'a str)> {
    let mut groups = Groups::new();
    for (id, fingerprint) in gone {
        let scope = kind.scope(id);
        let scope = if kind.in_plane() {
            planes.get(scope).copied().unwrap_or(scope)
        } else {
            scope
        };
        groups.entry((scope, fingerprint)).or_default().0.push(id);
    }
    for (id, fingerprint) in fresh {
        groups
            .entry((kind.scope(id), fingerprint))
            .or_default()
            .1
            .push(id);
    }
    groups
        .into_values()
        .filter_map(|group| match group {
            (from, to) if from.len() == 1 && to.len() == 1 => Some((from[0], to[0])),
            _ => None,
        })
        .collect()
}

/// Whether the allowlist accepts the removal of `id`: an entry removes it,
/// or renames it to an ID of the new world; or, for a zone, anchor or
/// stairwell of a removed plane, the plane's entry removes the plane, or
/// renames it to a plane of the new world that has the same local ID; or,
/// for an object uid whose object was on a removed plane, the plane's entry
/// removes the plane.
fn allowed(kind: Kind, id: &str, allowlist: &Allowlist, old: &Snapshot, new: &Snapshot) -> bool {
    match allowlist.get(kind, id) {
        Some(Intent::Remove) => return true,
        Some(Intent::Rename(to)) if new.contains(kind, to) => return true,
        _ => {}
    }
    if kind == Kind::Object {
        // A uid is the world's, not its plane's, so a renamed plane keeps
        // its objects' uids and accepts no removal of one.
        return old.object_plane(id).is_some_and(|plane| {
            !new.contains(Kind::Plane, plane)
                && matches!(allowlist.get(Kind::Plane, plane), Some(Intent::Remove))
        });
    }
    let Some((plane, local)) = id.split_once('/').filter(|_| kind.in_plane()) else {
        return false;
    };
    if new.contains(Kind::Plane, plane) {
        return false;
    }
    match allowlist.get(Kind::Plane, plane) {
        Some(Intent::Remove) => true,
        Some(Intent::Rename(to)) => {
            new.contains(Kind::Plane, to) && new.contains(kind, &format!("{to}/{local}"))
        }
        None => false,
    }
}

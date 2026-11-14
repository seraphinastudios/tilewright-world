// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The references of a world: what each one names, from where, and where
//! in which file it is written.
//!
//! [`load_indexed_in`](crate::load_indexed_in) loads a tree as
//! [`load_edited_in`](crate::load_edited_in) does and also returns a
//! [`ReferenceIndex`] of every reference the loader checked or followed.
//! The other loads record nothing.
//!
//! A reference is a value that names something else of the world by its
//! identity: an object's prototype or render layer, a portal's plane and
//! anchor, a value of a schema `reference` field, a zone's parent, a
//! plane's default zone, a palette entry, a stairwell step's prototype,
//! what a transitions document names, a schema field's definition type,
//! a profile ground layer's render layer, and the paths and identities
//! by which the manifest, the profile and the `defs/` layout tie files
//! together (see [`Role`]). Values of `json` fields and `custom` objects
//! are not looked into, so a name written there is not a reference here,
//! and neither is anything a game's own code reads from them. Nothing in
//! the format references a stairwell or an object uid, so neither is
//! ever a [`Target`].
//!
//! A declaration that names another item is a reference: a definition's
//! `type` names its definition type, and a chunk's `plane` names its
//! plane. An item's own identity, such as a plane's `id` or a
//! definition's `id`, is the item rather than a reference to it, so an
//! item's own files and identity are found from the [`World`](crate::World)
//! (its key, [`Chunk::path`](crate::Chunk::path), its plane's
//! directory), not from the index.
//!
//! An indexed load records nothing once the load has a problem, since it
//! then returns diagnostics and no index, and it refuses, with a
//! `source.budget` diagnostic, a tree whose references, strings or holders
//! would not fit the index's 32-bit numbering.

use crate::schema::{FieldPath, FieldType, Schema};
use std::{cmp::Ordering, collections::HashMap, fmt, ops::Range, sync::Arc};

/// A chunk's key in [`Plane::chunks`](crate::Plane::chunks):
/// `(layer, cx, cy)`.
pub type ChunkKey = (i32, i64, i64);

/// What a reference names.
///
/// Identities are as the source writes them. Anchors and zones belong to
/// a plane, so they are qualified by the plane's ID; a portal that omits
/// its plane names an anchor of its own plane (see
/// [`Role::PortalAnchor`]).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Target<'a> {
    /// The definition `defs/<kind>/<id>.jsonc`.
    Definition { kind: &'a str, id: &'a str },
    /// A definition type the profile declares.
    DefinitionType(&'a str),
    /// A plane the manifest lists.
    Plane(&'a str),
    /// An anchor of a plane.
    Anchor { plane: &'a str, id: &'a str },
    /// A zone of a plane.
    Zone { plane: &'a str, id: &'a str },
    /// A render layer of the profile.
    RenderLayer(&'a str),
    /// A ground layer of the profile.
    GroundLayer(&'a str),
    /// A terrain of the profile's transitions document.
    Terrain(&'a str),
    /// A file, by its root-relative `/`-separated path, as the path that
    /// names it resolves.
    File(&'a str),
}

/// Which kind of reference it is: what holds it, and what it is for.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Role<'a> {
    /// The manifest's `profile` path, to the profile document.
    ManifestProfile,
    /// An entry of the manifest's `planes`, to a plane.
    ManifestPlane,
    /// A profile `ground_layers` entry, to the render layer it must be.
    ProfileGroundLayer,
    /// A schema path of the profile (a definition type's `schema`,
    /// `object_fields` or `zone_properties`), to the schema document.
    ProfileSchema,
    /// The profile's `transitions` path, to the transitions document.
    ProfileTransitions,
    /// A `reference` field type of a schema, to its definition type.
    /// `field` is the field's path in the schema: names joined by `.`,
    /// with `[]` for a list's items, as in `loot.entries[].item`.
    SchemaTarget { field: &'a str },
    /// A definition's `type`, to the definition type its directory
    /// names.
    DefinitionType,
    /// A value of a schema `reference` field: of a definition's `fields`,
    /// an object's `fields` or a zone's `properties`. `field` is the
    /// field's path in the schema, as for [`Role::SchemaTarget`]; the
    /// value's span tells which list item holds it.
    Field { field: &'a str },
    /// A zone's `parent`.
    ZoneParent,
    /// A plane's `default_zone`.
    DefaultZone,
    /// An entry of a chunk's ground grid palette for the ground layer
    /// `layer`, to the prototype it paints.
    GroundPalette { layer: &'a str },
    /// An entry of a chunk's zone grid palette.
    ZonePalette,
    /// A chunk's `plane`, which names the plane the chunk belongs to.
    ChunkPlane,
    /// An object's `proto`.
    Proto,
    /// An object's `layer`, to a render layer.
    Layer,
    /// A portal's `plane`.
    PortalPlane,
    /// A portal's `anchor`. When the portal omits its plane,
    /// `implicit_plane` is `true`, the target's plane is the portal's own,
    /// and the span is the anchor's: no `plane` is written.
    PortalAnchor { implicit_plane: bool },
    /// A stairwell step's `proto`.
    StairwellStep,
    /// The transitions document's terrain `layer`, to a ground layer.
    TerrainLayer,
    /// An entry of a terrain's `protos`.
    TerrainProto,
    /// A transition's `layer`, to the ground layer it draws into.
    TransitionLayer,
    /// An entry of a transition's `with`, to a terrain.
    TransitionBlend,
    /// A prototype of a transition's `tiles`.
    TransitionTile,
}

/// What holds a reference.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Holder<'a> {
    /// `world.jsonc`.
    Manifest,
    /// The profile document.
    Profile,
    /// A schema document, by its root-relative path.
    Schema { path: &'a str },
    /// The profile's transitions document.
    Transitions,
    /// A definition.
    Definition { kind: &'a str, id: &'a str },
    /// A plane's `plane.jsonc`.
    Plane { plane: &'a str },
    /// A zone of a plane's `zones.jsonc`.
    Zone { plane: &'a str, zone: &'a str },
    /// A stairwell of a plane's `stairwells.jsonc`.
    Stairwell { plane: &'a str, stairwell: &'a str },
    /// A chunk itself: its grid palettes.
    Chunk { plane: &'a str, chunk: ChunkKey },
    /// An object of a chunk, inventory entries included. `ordinal` is the
    /// object's position in [`Chunk::depth_first`](crate::Chunk::depth_first),
    /// its index in the chunk.
    Object {
        plane: &'a str,
        chunk: ChunkKey,
        ordinal: usize,
    },
}

/// The references of one load, interned, with lookups by target and by
/// holder; see the [module documentation](self) for what is a reference.
///
/// Every reference is kept once. [`ReferenceIndex::iter`] and
/// [`ReferenceIndex::to`] give references grouped by target, targets in
/// a fixed order (by kind, then by their identities' bytes), and within a
/// target in source order: by file path, then by span. [`ReferenceIndex::from`]
/// gives a holder's references in source order. Both lookups are binary
/// searches, so their cost does not grow with the references they skip.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct ReferenceIndex {
    /// Every string, sorted and unique, so that comparing two symbols
    /// compares their strings.
    names: Vec<Box<str>>,
    /// Every holder, sorted.
    holders: Vec<HolderEntry>,
    /// Every reference, sorted by target.
    records: Vec<Record>,
    /// `records` positions, sorted by holder.
    by_holder: Vec<u32>,
}

/// One reference of a [`ReferenceIndex`].
#[derive(Clone, Copy)]
pub struct Reference<'i> {
    index: &'i ReferenceIndex,
    record: &'i Record,
}

impl<'i> Reference<'i> {
    /// What it names.
    #[must_use]
    pub fn target(&self) -> Target<'i> {
        self.index.target(&self.record.target())
    }

    /// Which kind of reference it is.
    #[must_use]
    pub fn role(&self) -> Role<'i> {
        self.index.role(self.record)
    }

    /// What holds it.
    #[must_use]
    pub fn holder(&self) -> Holder<'i> {
        self.index.holder(self.record.holder, self.record.ordinal)
    }

    /// The root-relative path of the file it is written in.
    #[must_use]
    pub fn file(&self) -> &'i str {
        self.index.holder_file(self.record.holder)
    }

    /// Its byte span in the file, as the file was read: the value that
    /// names the target, quotes included. For a portal anchor whose
    /// plane is implicit, it is the anchor's.
    #[must_use]
    pub fn span(&self) -> Range<usize> {
        widen(self.record.start)..widen(self.record.end)
    }
}

impl fmt::Debug for Reference<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Reference")
            .field("target", &self.target())
            .field("role", &self.role())
            .field("holder", &self.holder())
            .field("file", &self.file())
            .field("span", &self.span())
            .finish()
    }
}

impl fmt::Debug for ReferenceIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

impl ReferenceIndex {
    /// How many references there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Every reference, grouped by target, each group in source order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Reference<'_>> + '_ {
        self.records.iter().map(|record| Reference {
            index: self,
            record,
        })
    }

    /// The references to `target`, in source order: none when nothing
    /// names it, or when it does not exist.
    pub fn to(&self, target: Target<'_>) -> impl ExactSizeIterator<Item = Reference<'_>> + '_ {
        let found = self.target_key(target).map_or(&[][..], |key| {
            let start = self.records.partition_point(|record| record.target() < key);
            let length = self.records[start..].partition_point(|record| record.target() == key);
            &self.records[start..start + length]
        });
        found.iter().map(|record| Reference {
            index: self,
            record,
        })
    }

    /// The references `holder` holds, in source order. An object's are
    /// its own, not its inventory's; a chunk's are its palettes', not its
    /// objects'.
    pub fn from(&self, holder: Holder<'_>) -> impl ExactSizeIterator<Item = Reference<'_>> + '_ {
        let found = self
            .holder_key(holder)
            .map_or(&[][..], |(holder, ordinal)| {
                let key = (holder, ordinal);
                let at = |position: &u32| {
                    let record = &self.records[widen(*position)];
                    (record.holder, record.ordinal)
                };
                let start = self
                    .by_holder
                    .partition_point(|position| at(position) < key);
                let length =
                    self.by_holder[start..].partition_point(|position| at(position) == key);
                &self.by_holder[start..start + length]
            });
        found.iter().map(|position| Reference {
            index: self,
            record: &self.records[widen(*position)],
        })
    }

    /// The bytes the index holds, for measurement: its tables and its
    /// strings.
    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        self.names.capacity() * size_of::<Box<str>>()
            + self.names.iter().map(|name| name.len()).sum::<usize>()
            + self.holders.capacity() * size_of::<HolderEntry>()
            + self.records.capacity() * size_of::<Record>()
            + self.by_holder.capacity() * size_of::<u32>()
    }

    fn name(&self, symbol: u32) -> &str {
        self.names.get(widen(symbol)).map_or("", |name| name)
    }

    fn symbol(&self, text: &str) -> Option<u32> {
        let position = self
            .names
            .binary_search_by(|name| (**name).cmp(text))
            .ok()?;
        u32::try_from(position).ok()
    }

    fn target(&self, key: &TargetKey) -> Target<'_> {
        let (a, b) = (self.name(key.a), self.name(key.b));
        match key.kind {
            TargetKind::Definition => Target::Definition { kind: a, id: b },
            TargetKind::DefinitionType => Target::DefinitionType(a),
            TargetKind::Plane => Target::Plane(a),
            TargetKind::Anchor => Target::Anchor { plane: a, id: b },
            TargetKind::Zone => Target::Zone { plane: a, id: b },
            TargetKind::RenderLayer => Target::RenderLayer(a),
            TargetKind::GroundLayer => Target::GroundLayer(a),
            TargetKind::Terrain => Target::Terrain(a),
            TargetKind::File => Target::File(a),
        }
    }

    fn target_key(&self, target: Target<'_>) -> Option<TargetKey> {
        let (kind, a, b) = split_target(target);
        Some(TargetKey {
            kind,
            a: self.symbol(a)?,
            b: if b.is_empty() { 0 } else { self.symbol(b)? },
        })
    }

    fn role(&self, record: &Record) -> Role<'_> {
        let argument = self.name(record.argument);
        match record.role {
            RoleKind::ManifestProfile => Role::ManifestProfile,
            RoleKind::ManifestPlane => Role::ManifestPlane,
            RoleKind::ProfileGroundLayer => Role::ProfileGroundLayer,
            RoleKind::ProfileSchema => Role::ProfileSchema,
            RoleKind::ProfileTransitions => Role::ProfileTransitions,
            RoleKind::SchemaTarget => Role::SchemaTarget { field: argument },
            RoleKind::DefinitionType => Role::DefinitionType,
            RoleKind::Field => Role::Field { field: argument },
            RoleKind::ZoneParent => Role::ZoneParent,
            RoleKind::DefaultZone => Role::DefaultZone,
            RoleKind::GroundPalette => Role::GroundPalette { layer: argument },
            RoleKind::ZonePalette => Role::ZonePalette,
            RoleKind::ChunkPlane => Role::ChunkPlane,
            RoleKind::Proto => Role::Proto,
            RoleKind::Layer => Role::Layer,
            RoleKind::PortalPlane => Role::PortalPlane,
            RoleKind::PortalAnchor => Role::PortalAnchor {
                implicit_plane: record.argument != 0,
            },
            RoleKind::StairwellStep => Role::StairwellStep,
            RoleKind::TerrainLayer => Role::TerrainLayer,
            RoleKind::TerrainProto => Role::TerrainProto,
            RoleKind::TransitionLayer => Role::TransitionLayer,
            RoleKind::TransitionBlend => Role::TransitionBlend,
            RoleKind::TransitionTile => Role::TransitionTile,
        }
    }

    fn holder_entry(&self, holder: u32) -> Option<&HolderEntry> {
        self.holders.get(widen(holder))
    }

    fn holder_file(&self, holder: u32) -> &str {
        self.holder_entry(holder)
            .map_or("", |entry| self.name(entry.file))
    }

    fn holder(&self, holder: u32, ordinal: u32) -> Holder<'_> {
        let Some(entry) = self.holder_entry(holder) else {
            return Holder::Manifest;
        };
        let (a, b) = (self.name(entry.key.a), self.name(entry.key.b));
        match entry.key.kind {
            HolderKind::Manifest => Holder::Manifest,
            HolderKind::Profile => Holder::Profile,
            HolderKind::Schema => Holder::Schema { path: a },
            HolderKind::Transitions => Holder::Transitions,
            HolderKind::Definition => Holder::Definition { kind: a, id: b },
            HolderKind::Plane => Holder::Plane { plane: a },
            HolderKind::Zone => Holder::Zone { plane: a, zone: b },
            HolderKind::Stairwell => Holder::Stairwell {
                plane: a,
                stairwell: b,
            },
            HolderKind::Chunk if ordinal == NO_ORDINAL => Holder::Chunk {
                plane: a,
                chunk: entry.key.chunk,
            },
            HolderKind::Chunk => Holder::Object {
                plane: a,
                chunk: entry.key.chunk,
                ordinal: widen(ordinal),
            },
        }
    }

    /// The position of `holder` in `holders`, and the ordinal its
    /// references carry.
    fn holder_key(&self, holder: Holder<'_>) -> Option<(u32, u32)> {
        let (kind, a, b, chunk, ordinal) = split_holder(holder);
        let ordinal = match ordinal {
            Some(ordinal) => u32::try_from(ordinal).ok().filter(|o| *o != NO_ORDINAL)?,
            None => NO_ORDINAL,
        };
        let key = HolderKey {
            kind,
            a: if a.is_empty() { 0 } else { self.symbol(a)? },
            b: if b.is_empty() { 0 } else { self.symbol(b)? },
            chunk,
        };
        let position = self
            .holders
            .binary_search_by(|entry| entry.key.cmp(&key))
            .ok()?;
        Some((u32::try_from(position).ok()?, ordinal))
    }
}

/// `usize` from `u32`, which every supported platform holds.
fn widen(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The ordinal of a reference a chunk holds itself, not one of its
/// objects.
const NO_ORDINAL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum TargetKind {
    Definition,
    DefinitionType,
    Plane,
    Anchor,
    Zone,
    RenderLayer,
    GroundLayer,
    Terrain,
    File,
}

/// A target's kind and its one or two identities; the second is empty
/// for kinds that have one.
fn split_target(target: Target<'_>) -> (TargetKind, &str, &str) {
    match target {
        Target::Definition { kind, id } => (TargetKind::Definition, kind, id),
        Target::DefinitionType(kind) => (TargetKind::DefinitionType, kind, ""),
        Target::Plane(plane) => (TargetKind::Plane, plane, ""),
        Target::Anchor { plane, id } => (TargetKind::Anchor, plane, id),
        Target::Zone { plane, id } => (TargetKind::Zone, plane, id),
        Target::RenderLayer(layer) => (TargetKind::RenderLayer, layer, ""),
        Target::GroundLayer(layer) => (TargetKind::GroundLayer, layer, ""),
        Target::Terrain(terrain) => (TargetKind::Terrain, terrain, ""),
        Target::File(path) => (TargetKind::File, path, ""),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum RoleKind {
    ManifestProfile,
    ManifestPlane,
    ProfileGroundLayer,
    ProfileSchema,
    ProfileTransitions,
    SchemaTarget,
    DefinitionType,
    Field,
    ZoneParent,
    DefaultZone,
    GroundPalette,
    ZonePalette,
    ChunkPlane,
    Proto,
    Layer,
    PortalPlane,
    PortalAnchor,
    StairwellStep,
    TerrainLayer,
    TerrainProto,
    TransitionLayer,
    TransitionBlend,
    TransitionTile,
}

/// What a role carries: a string, a flag, or nothing.
enum Argument<'a> {
    None,
    Text(&'a str),
    Flag(bool),
}

fn split_role(role: Role<'_>) -> (RoleKind, Argument<'_>) {
    match role {
        Role::ManifestProfile => (RoleKind::ManifestProfile, Argument::None),
        Role::ManifestPlane => (RoleKind::ManifestPlane, Argument::None),
        Role::ProfileGroundLayer => (RoleKind::ProfileGroundLayer, Argument::None),
        Role::ProfileSchema => (RoleKind::ProfileSchema, Argument::None),
        Role::ProfileTransitions => (RoleKind::ProfileTransitions, Argument::None),
        Role::SchemaTarget { field } => (RoleKind::SchemaTarget, Argument::Text(field)),
        Role::DefinitionType => (RoleKind::DefinitionType, Argument::None),
        Role::Field { field } => (RoleKind::Field, Argument::Text(field)),
        Role::ZoneParent => (RoleKind::ZoneParent, Argument::None),
        Role::DefaultZone => (RoleKind::DefaultZone, Argument::None),
        Role::GroundPalette { layer } => (RoleKind::GroundPalette, Argument::Text(layer)),
        Role::ZonePalette => (RoleKind::ZonePalette, Argument::None),
        Role::ChunkPlane => (RoleKind::ChunkPlane, Argument::None),
        Role::Proto => (RoleKind::Proto, Argument::None),
        Role::Layer => (RoleKind::Layer, Argument::None),
        Role::PortalPlane => (RoleKind::PortalPlane, Argument::None),
        Role::PortalAnchor { implicit_plane } => {
            (RoleKind::PortalAnchor, Argument::Flag(implicit_plane))
        }
        Role::StairwellStep => (RoleKind::StairwellStep, Argument::None),
        Role::TerrainLayer => (RoleKind::TerrainLayer, Argument::None),
        Role::TerrainProto => (RoleKind::TerrainProto, Argument::None),
        Role::TransitionLayer => (RoleKind::TransitionLayer, Argument::None),
        Role::TransitionBlend => (RoleKind::TransitionBlend, Argument::None),
        Role::TransitionTile => (RoleKind::TransitionTile, Argument::None),
    }
}

/// Whether a role's argument is a symbol, which interning renumbers.
const fn argument_is_symbol(role: RoleKind) -> bool {
    matches!(
        role,
        RoleKind::SchemaTarget | RoleKind::Field | RoleKind::GroundPalette
    )
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum HolderKind {
    Manifest,
    Profile,
    Schema,
    Transitions,
    Definition,
    Plane,
    Zone,
    Stairwell,
    /// A chunk, and its objects, which their ordinal tells apart.
    Chunk,
}

/// A holder's kind, its identities (empty where it has fewer), its chunk
/// key (zero where it has none) and, for an object, its ordinal.
fn split_holder(holder: Holder<'_>) -> (HolderKind, &str, &str, ChunkKey, Option<usize>) {
    match holder {
        Holder::Manifest => (HolderKind::Manifest, "", "", (0, 0, 0), None),
        Holder::Profile => (HolderKind::Profile, "", "", (0, 0, 0), None),
        Holder::Schema { path } => (HolderKind::Schema, path, "", (0, 0, 0), None),
        Holder::Transitions => (HolderKind::Transitions, "", "", (0, 0, 0), None),
        Holder::Definition { kind, id } => (HolderKind::Definition, kind, id, (0, 0, 0), None),
        Holder::Plane { plane } => (HolderKind::Plane, plane, "", (0, 0, 0), None),
        Holder::Zone { plane, zone } => (HolderKind::Zone, plane, zone, (0, 0, 0), None),
        Holder::Stairwell { plane, stairwell } => {
            (HolderKind::Stairwell, plane, stairwell, (0, 0, 0), None)
        }
        Holder::Chunk { plane, chunk } => (HolderKind::Chunk, plane, "", chunk, None),
        Holder::Object {
            plane,
            chunk,
            ordinal,
        } => (HolderKind::Chunk, plane, "", chunk, Some(ordinal)),
    }
}

/// A target by kind and symbols; `b` is 0 for kinds with one identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct TargetKey {
    kind: TargetKind,
    a: u32,
    b: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct HolderKey {
    kind: HolderKind,
    a: u32,
    b: u32,
    chunk: ChunkKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HolderEntry {
    key: HolderKey,
    /// The file the holder's references are written in.
    file: u32,
}

/// One reference: 32 bytes. The target's parts are fields of their own,
/// so that no padding comes between them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Record {
    kind: TargetKind,
    role: RoleKind,
    a: u32,
    b: u32,
    /// A symbol, or a flag, by role: see [`argument_is_symbol`].
    argument: u32,
    /// A position in `holders`.
    holder: u32,
    /// The object's ordinal in its chunk, or [`NO_ORDINAL`].
    ordinal: u32,
    start: u32,
    end: u32,
}

impl Record {
    const fn target(&self) -> TargetKey {
        TargetKey {
            kind: self.kind,
            a: self.a,
            b: self.b,
        }
    }
}

/// One reference as the loader checks or follows it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Site<'a> {
    pub target: Target<'a>,
    pub role: Role<'a>,
    pub holder: Holder<'a>,
    /// The file it is written in, which its holder's references share.
    pub file: &'a str,
    pub start: usize,
    pub end: usize,
}

/// Most references, distinct strings or holders an index holds: each is
/// numbered by a `u32`, and `u32::MAX` is kept as a marker.
const MAXIMUM_COUNT: u32 = u32::MAX - 1;

#[cfg(test)]
thread_local! {
    /// References recorded on this thread, so tests can tell when a load
    /// records and when it stops.
    pub(crate) static RECORDED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// A lower count limit for the recorders a test makes on its thread.
    pub(crate) static TEST_MAXIMUM_COUNT: std::cell::Cell<Option<u32>> =
        const { std::cell::Cell::new(None) };
}

/// Collects the references of one load, interning their strings as it
/// goes; [`Recorder::finish`] sorts them into a [`ReferenceIndex`]. A
/// reference that would not fit the index's packed fields makes it
/// refuse instead ([`Recorder::overflowed`]): nothing is ever cut short.
pub(crate) struct Recorder {
    symbols: HashMap<Box<str>, u32>,
    names: Vec<Box<str>>,
    holders: HashMap<HolderKey, u32>,
    holder_entries: Vec<HolderEntry>,
    records: Vec<Record>,
    /// The interned field path of each `reference` field type a value had,
    /// by the address of the schema that holds the type and of the type.
    /// Each entry keeps its schema alive, so while the recorder lives no
    /// other schema can take that address, and the types inside it, which
    /// an `Arc` never moves or changes, keep theirs: a key cannot go stale.
    fields: HashMap<(usize, usize), (Arc<Schema>, u32)>,
    /// Most records, strings or holders.
    maximum: u32,
    /// Something did not fit, and nothing more is recorded.
    overflowed: bool,
}

impl Default for Recorder {
    fn default() -> Self {
        #[cfg(test)]
        let maximum = TEST_MAXIMUM_COUNT.get().unwrap_or(MAXIMUM_COUNT);
        #[cfg(not(test))]
        let maximum = MAXIMUM_COUNT;
        // Symbol 0 is the empty string, which absent parts name, whatever
        // is recorded first.
        Self {
            symbols: HashMap::from([(Box::from(""), 0)]),
            names: vec![Box::from("")],
            holders: HashMap::new(),
            holder_entries: Vec::new(),
            records: Vec::new(),
            fields: HashMap::new(),
            maximum,
            overflowed: false,
        }
    }
}

/// `value` as a `u32` below `u32::MAX`, which is kept as a marker; `None`
/// when it does not fit.
pub(crate) fn packed(value: usize) -> Option<u32> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value <= MAXIMUM_COUNT)
}

/// `u32` from a count the recorder has kept within [`MAXIMUM_COUNT`].
fn narrow(value: usize) -> u32 {
    packed(value).unwrap_or(MAXIMUM_COUNT)
}

impl Recorder {
    /// Whether a reference did not fit, so the index is refused.
    pub const fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// `value` packed, or `None` after noting that it did not fit.
    fn fit(&mut self, value: Option<u32>) -> Option<u32> {
        if value.is_none() {
            self.overflowed = true;
        }
        value
    }

    /// The next number of a table of `length` entries.
    fn next(&mut self, length: usize) -> Option<u32> {
        let maximum = self.maximum;
        self.fit(packed(length).filter(|next| *next < maximum))
    }

    fn intern(&mut self, text: &str) -> Option<u32> {
        if let Some(symbol) = self.symbols.get(text) {
            return Some(*symbol);
        }
        let symbol = self.next(self.names.len())?;
        self.names.push(text.into());
        self.symbols.insert(text.into(), symbol);
        Some(symbol)
    }

    /// Interns a part that may be empty, which symbol 0, the empty string,
    /// stands for.
    fn intern_part(&mut self, text: &str) -> Option<u32> {
        if text.is_empty() {
            Some(0)
        } else {
            self.intern(text)
        }
    }

    pub fn record(&mut self, site: &Site<'_>) {
        let _ = self.try_record(site, None);
    }

    /// Records a value of the `reference` field type `field_type` of
    /// `schema`, at field path `path`, which is written out and interned
    /// only the first time a value of that type is recorded. `site`'s role
    /// is [`Role::Field`]; its path is taken from here.
    pub fn record_field(
        &mut self,
        site: &Site<'_>,
        (schema, field_type): (&Arc<Schema>, &FieldType),
        path: &FieldPath<'_>,
    ) {
        if self.overflowed {
            return;
        }
        let key = (
            Arc::as_ptr(schema).addr(),
            std::ptr::from_ref(field_type).addr(),
        );
        let symbol = if let Some((_, symbol)) = self.fields.get(&key) {
            *symbol
        } else {
            let Some(symbol) = self.intern(&path.render()) else {
                return;
            };
            self.fields.insert(key, (Arc::clone(schema), symbol));
            symbol
        };
        let _ = self.try_record(site, Some(symbol));
    }

    /// Records `site`, with `argument` as its role's symbol when given.
    /// `None` when it did not fit, which refuses the index.
    fn try_record(&mut self, site: &Site<'_>, argument: Option<u32>) -> Option<()> {
        if self.overflowed {
            return None;
        }
        let (kind, a, b) = split_target(site.target);
        let target = TargetKey {
            kind,
            a: self.intern(a)?,
            b: self.intern_part(b)?,
        };
        let (role, text) = split_role(site.role);
        let argument = match (argument, text) {
            (Some(symbol), _) => symbol,
            (None, Argument::None) => 0,
            (None, Argument::Text(text)) => self.intern(text)?,
            (None, Argument::Flag(flag)) => u32::from(flag),
        };
        let (holder_kind, holder_a, holder_b, chunk, ordinal) = split_holder(site.holder);
        let key = HolderKey {
            kind: holder_kind,
            a: self.intern_part(holder_a)?,
            b: self.intern_part(holder_b)?,
            chunk,
        };
        let file = self.intern(site.file)?;
        let holder = if let Some(holder) = self.holders.get(&key) {
            *holder
        } else {
            let position = self.next(self.holder_entries.len())?;
            self.holder_entries.push(HolderEntry { key, file });
            self.holders.insert(key, position);
            position
        };
        let ordinal = match ordinal {
            None => NO_ORDINAL,
            // An ordinal is below the marker, which means none.
            Some(ordinal) => self.fit(packed(ordinal).filter(|o| *o < NO_ORDINAL))?,
        };
        let (start, end) = (packed(site.start), packed(site.end));
        let (start, end) = (self.fit(start)?, self.fit(end)?);
        self.next(self.records.len())?;
        self.records.push(Record {
            kind: target.kind,
            a: target.a,
            b: target.b,
            role,
            argument,
            holder,
            ordinal,
            start,
            end,
        });
        #[cfg(test)]
        RECORDED.with(|recorded| recorded.set(recorded.get() + 1));
        Some(())
    }

    /// The index: strings sorted, every symbol renumbered to match, and
    /// the references sorted by target and by holder.
    pub fn finish(self) -> ReferenceIndex {
        let Self {
            names,
            holder_entries,
            records,
            ..
        } = self;
        let (names, renumbered) = sorted(names);
        let symbol = |old: u32| renumbered.get(widen(old)).copied().unwrap_or(0);

        let renumber_holder = |key: HolderKey| HolderKey {
            a: symbol(key.a),
            b: symbol(key.b),
            ..key
        };
        let mut holders: Vec<(u32, HolderEntry)> = holder_entries
            .into_iter()
            .enumerate()
            .map(|(old, entry)| {
                (
                    narrow(old),
                    HolderEntry {
                        key: renumber_holder(entry.key),
                        file: symbol(entry.file),
                    },
                )
            })
            .collect();
        holders.sort_by_key(|(_, entry)| entry.key);
        let mut holder_position = vec![0; holders.len()];
        for (new, (old, _)) in holders.iter().enumerate() {
            holder_position[widen(*old)] = narrow(new);
        }
        let holders: Vec<HolderEntry> = holders.into_iter().map(|(_, entry)| entry).collect();

        let mut records: Vec<Record> = records
            .into_iter()
            .map(|record| Record {
                a: symbol(record.a),
                b: symbol(record.b),
                argument: if argument_is_symbol(record.role) {
                    symbol(record.argument)
                } else {
                    record.argument
                },
                holder: holder_position
                    .get(widen(record.holder))
                    .copied()
                    .unwrap_or(0),
                ..record
            })
            .collect();
        let file = |record: &Record| {
            holders
                .get(widen(record.holder))
                .map_or(0, |entry| entry.file)
        };
        // Within a target: source order, then whatever else tells two
        // references apart, so the order is total.
        let source_order = |a: &Record, b: &Record| -> Ordering {
            (
                file(a),
                a.start,
                a.end,
                a.role,
                a.argument,
                a.holder,
                a.ordinal,
            )
                .cmp(&(
                    file(b),
                    b.start,
                    b.end,
                    b.role,
                    b.argument,
                    b.holder,
                    b.ordinal,
                ))
        };
        records.sort_by(|a, b| a.target().cmp(&b.target()).then_with(|| source_order(a, b)));
        let mut by_holder: Vec<u32> = (0..narrow(records.len())).collect();
        by_holder.sort_by(|a, b| {
            let (a, b) = (&records[widen(*a)], &records[widen(*b)]);
            (a.holder, a.ordinal)
                .cmp(&(b.holder, b.ordinal))
                .then_with(|| source_order(a, b))
                .then_with(|| a.target().cmp(&b.target()))
        });
        // Recording grew the tables by doubling; the index keeps only what
        // it holds.
        records.shrink_to_fit();
        ReferenceIndex {
            names,
            holders,
            records,
            by_holder,
        }
    }
}

/// `names` sorted, and the new position of each old one.
fn sorted(names: Vec<Box<str>>) -> (Vec<Box<str>>, Vec<u32>) {
    let mut order: Vec<u32> = (0..narrow(names.len())).collect();
    order.sort_by(|a, b| names[widen(*a)].cmp(&names[widen(*b)]));
    let mut renumbered = vec![0; names.len()];
    for (new, old) in order.iter().enumerate() {
        renumbered[widen(*old)] = narrow(new);
    }
    let mut slots: Vec<Option<Box<str>>> = names.into_iter().map(Some).collect();
    let mut names: Vec<Box<str>> = order
        .iter()
        .map(|old| slots[widen(*old)].take().unwrap_or_default())
        .collect();
    names.shrink_to_fit();
    (names, renumbered)
}

/// The diagnostic message for a reference that does not resolve. Each
/// role words it as the loader always has.
pub(crate) fn message(site: &Site<'_>) -> String {
    match (site.role, site.target) {
        (Role::ZonePalette, Target::Zone { id, .. }) => {
            format!("zone \"{id}\" is not in this plane's zones.jsonc")
        }
        (Role::ZoneParent, Target::Zone { id: parent, .. }) => {
            let zone = match site.holder {
                Holder::Zone { zone, .. } => zone,
                _ => "",
            };
            format!("zone \"{zone}\" has unknown parent \"{parent}\"")
        }
        (Role::DefaultZone, Target::Zone { id, .. }) => {
            format!("default zone \"{id}\" is not in zones.jsonc")
        }
        (Role::ProfileGroundLayer, Target::RenderLayer(layer)) => {
            format!("ground layer \"{layer}\" must be a render layer other than height or zone")
        }
        (Role::SchemaTarget { field }, Target::DefinitionType(kind)) => {
            format!("field \"{field}\" references unknown definition type \"{kind}\"")
        }
        (_, Target::Definition { kind, id }) => format!("no {kind} definition \"{id}\""),
        (_, Target::Plane(plane)) => format!("no plane \"{plane}\""),
        (_, Target::Anchor { plane, id }) => format!("no anchor \"{id}\" in plane \"{plane}\""),
        (_, Target::RenderLayer(layer)) => {
            format!("\"{layer}\" is not a render layer of the profile")
        }
        (_, Target::GroundLayer(layer)) => {
            format!("\"{layer}\" is not a ground layer of the profile")
        }
        (_, Target::Terrain(terrain)) => format!("no terrain \"{terrain}\""),
        (_, Target::Zone { plane, id }) => format!("no zone \"{id}\" in plane \"{plane}\""),
        (_, Target::DefinitionType(kind)) => format!("no definition type \"{kind}\""),
        (_, Target::File(path)) => format!("no file {path}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Holder, Record, Recorder, Role, Site, Target};

    fn site<'a>(target: Target<'a>, holder: Holder<'a>, file: &'a str, start: usize) -> Site<'a> {
        Site {
            target,
            role: Role::Proto,
            holder,
            file,
            start,
            end: start + 3,
        }
    }

    #[test]
    fn a_value_is_packed_only_below_the_marker() {
        use super::packed;
        assert_eq!(packed(0), Some(0));
        let largest = usize::try_from(u32::MAX - 1).unwrap();
        assert_eq!(packed(largest), Some(u32::MAX - 1));
        assert_eq!(packed(largest + 1), None);
        assert_eq!(packed(usize::MAX), None);
    }

    #[test]
    fn a_record_that_does_not_fit_refuses_the_index() {
        let mut recorder = Recorder::default();
        recorder.record(&Site {
            start: usize::MAX,
            ..site(Target::Plane("p"), Holder::Manifest, "world.jsonc", 0)
        });
        assert!(recorder.overflowed());
        // Nothing more is recorded once one did not fit.
        recorder.record(&site(
            Target::Plane("p"),
            Holder::Manifest,
            "world.jsonc",
            0,
        ));
        assert!(recorder.overflowed());
        assert_eq!(recorder.finish().len(), 0);
        let mut ordinal = Recorder::default();
        ordinal.record(&site(
            Target::Plane("p"),
            Holder::Object {
                plane: "p",
                chunk: (0, 0, 0),
                ordinal: usize::try_from(u32::MAX).unwrap(),
            },
            "c.jsonc",
            0,
        ));
        assert!(ordinal.overflowed());
    }

    /// A field value recorded first must not take symbol 0, which absent
    /// parts name.
    #[test]
    fn a_field_recorded_first_keeps_symbol_zero_empty() {
        use crate::schema::{FieldPath, FieldType, Schema};
        use std::sync::Arc;
        let schema = Arc::new(Schema::new("s.jsonc".into()));
        let field_type = FieldType::Reference {
            target: "proto".into(),
        };
        let root = FieldPath::Root;
        let path = FieldPath::Field(&root, "f");
        let holder = Holder::Object {
            plane: "p",
            chunk: (0, 0, 0),
            ordinal: 0,
        };
        let mut recorder = Recorder::default();
        recorder.record_field(
            &Site {
                target: Target::Definition {
                    kind: "proto",
                    id: "x",
                },
                role: Role::Field { field: "" },
                holder,
                file: "c.jsonc",
                start: 1,
                end: 4,
            },
            (&schema, &field_type),
            &path,
        );
        recorder.record(&Site {
            target: Target::Plane("p"),
            role: Role::ChunkPlane,
            holder: Holder::Chunk {
                plane: "p",
                chunk: (0, 0, 0),
            },
            file: "c.jsonc",
            start: 10,
            end: 13,
        });
        let index = recorder.finish();
        let fields: Vec<Role<'_>> = index.from(holder).map(|found| found.role()).collect();
        assert_eq!(fields, [Role::Field { field: "f" }]);
        assert_eq!(index.to(Target::Plane("p")).count(), 1);
    }

    #[test]
    fn a_record_is_32_bytes() {
        assert_eq!(size_of::<Record>(), 32);
    }

    #[test]
    fn interning_sorts_strings_and_renumbers_every_symbol() {
        let mut recorder = Recorder::default();
        let chunk = |ordinal| Holder::Object {
            plane: "p",
            chunk: (0, 0, 0),
            ordinal,
        };
        // Recorded out of order, with strings first seen out of order.
        recorder.record(&site(
            Target::Definition {
                kind: "proto",
                id: "zeta",
            },
            chunk(1),
            "b.jsonc",
            40,
        ));
        recorder.record(&site(
            Target::Definition {
                kind: "proto",
                id: "alpha",
            },
            chunk(0),
            "b.jsonc",
            10,
        ));
        recorder.record(&site(
            Target::Definition {
                kind: "proto",
                id: "zeta",
            },
            Holder::Stairwell {
                plane: "p",
                stairwell: "s",
            },
            "a.jsonc",
            90,
        ));
        let index = recorder.finish();
        assert_eq!(index.len(), 3);
        let order: Vec<(Target<'_>, &str, usize)> = index
            .iter()
            .map(|reference| (reference.target(), reference.file(), reference.span().start))
            .collect();
        assert_eq!(
            order,
            [
                (
                    Target::Definition {
                        kind: "proto",
                        id: "alpha"
                    },
                    "b.jsonc",
                    10
                ),
                (
                    Target::Definition {
                        kind: "proto",
                        id: "zeta"
                    },
                    "a.jsonc",
                    90
                ),
                (
                    Target::Definition {
                        kind: "proto",
                        id: "zeta"
                    },
                    "b.jsonc",
                    40
                ),
            ]
        );
        let from: Vec<usize> = index
            .from(chunk(1))
            .map(|reference| reference.span().start)
            .collect();
        assert_eq!(from, [40]);
        assert_eq!(index.from(chunk(2)).len(), 0);
        assert_eq!(
            index
                .from(Holder::Chunk {
                    plane: "p",
                    chunk: (0, 0, 0)
                })
                .len(),
            0
        );
        assert_eq!(
            index
                .to(Target::Definition {
                    kind: "proto",
                    id: "missing"
                })
                .len(),
            0
        );
        // A string the index holds, as another kind of target.
        assert_eq!(index.to(Target::Plane("proto")).len(), 0);
    }
}

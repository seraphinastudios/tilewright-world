// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `references` and `impact`: lookups in the reference index the snapshot
//! keeps beside its world (docs/MCP.md, "References and impact").
//!
//! The index holds every reference the loader checked or followed, sorted
//! by target and by holder, so both lookups are binary searches. A
//! reference names another item; an item's own identity and files are the
//! world's, not the index's, so `impact` takes those from the world.

use crate::budget::{self, Output};
use crate::search;
use std::fmt::Write as _;
use tilewright_source::index::{ChunkKey, Holder, Reference, ReferenceIndex, Role, Target};
use tilewright_source::{Chunk, World, is_identifier, is_jsonc_path};

/// A kind of item `references` and `impact` name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemKind {
    Anchor,
    Chunk,
    Definition,
    DefinitionType,
    File,
    GroundLayer,
    Manifest,
    Object,
    Plane,
    Profile,
    RenderLayer,
    Schema,
    Stairwell,
    Terrain,
    Transitions,
    Zone,
}

impl ItemKind {
    pub const ALL: [Self; 16] = [
        Self::Anchor,
        Self::Chunk,
        Self::Definition,
        Self::DefinitionType,
        Self::File,
        Self::GroundLayer,
        Self::Manifest,
        Self::Object,
        Self::Plane,
        Self::Profile,
        Self::RenderLayer,
        Self::Schema,
        Self::Stairwell,
        Self::Terrain,
        Self::Transitions,
        Self::Zone,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Chunk => "chunk",
            Self::Definition => "definition",
            Self::DefinitionType => "definition_type",
            Self::File => "file",
            Self::GroundLayer => "ground_layer",
            Self::Manifest => "manifest",
            Self::Object => "object",
            Self::Plane => "plane",
            Self::Profile => "profile",
            Self::RenderLayer => "render_layer",
            Self::Schema => "schema",
            Self::Stairwell => "stairwell",
            Self::Terrain => "terrain",
            Self::Transitions => "transitions",
            Self::Zone => "zone",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// Whether `references` takes it with `direction` `to`: a kind of
    /// item a reference can name, and stairwells and objects, which none
    /// names.
    pub const fn takes_to(self) -> bool {
        !matches!(
            self,
            Self::Chunk | Self::Manifest | Self::Profile | Self::Schema | Self::Transitions
        )
    }

    /// Whether `references` takes it with `direction` `from`: a kind of
    /// item that holds references.
    pub const fn takes_from(self) -> bool {
        matches!(
            self,
            Self::Chunk
                | Self::Definition
                | Self::Manifest
                | Self::Object
                | Self::Plane
                | Self::Profile
                | Self::Schema
                | Self::Stairwell
                | Self::Transitions
                | Self::Zone
        )
    }

    /// Whether `impact` takes it.
    pub const fn takes_impact(self) -> bool {
        matches!(
            self,
            Self::Anchor
                | Self::Definition
                | Self::Object
                | Self::Plane
                | Self::RenderLayer
                | Self::Stairwell
                | Self::Terrain
                | Self::Zone
        )
    }
}

/// An item, by the ID an argument gives, checked for its shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Address<'a> {
    Anchor {
        plane: &'a str,
        id: &'a str,
    },
    Chunk(&'a str),
    Definition {
        kind: &'a str,
        id: &'a str,
    },
    DefinitionType(&'a str),
    File(&'a str),
    GroundLayer(&'a str),
    Manifest,
    /// An object by its uid.
    Object(&'a str),
    /// An object by its chunk's path and its index in the chunk, depth
    /// first: `<path>#<ordinal>`.
    ObjectAt {
        chunk: &'a str,
        ordinal: usize,
    },
    Plane(&'a str),
    Profile(&'a str),
    RenderLayer(&'a str),
    Schema(&'a str),
    Stairwell {
        plane: &'a str,
        id: &'a str,
    },
    Terrain(&'a str),
    Transitions(&'a str),
    Zone {
        plane: &'a str,
        id: &'a str,
    },
}

/// The manifest's path, which is the manifest's ID.
pub const MANIFEST: &str = "world.jsonc";

/// `owner/name`, both identifiers.
fn owned(id: &str) -> Option<(&str, &str)> {
    id.split_once('/')
        .filter(|(owner, name)| is_identifier(owner) && is_identifier(name))
}

/// A chunk's path split into its plane and key, when it is written as a
/// load gives it, `planes/<plane>/layers/<layer>/<x>_<y>.jsonc`.
fn chunk_path(path: &str) -> Option<(&str, ChunkKey)> {
    let rest = path.strip_prefix("planes/")?;
    let (plane, rest) = rest.split_once("/layers/")?;
    let (layer, name) = rest.split_once('/')?;
    let (x, y) = name.strip_suffix(".jsonc")?.split_once('_')?;
    let key = (layer.parse().ok()?, x.parse().ok()?, y.parse().ok()?);
    is_identifier(plane).then_some((plane, key))
}

/// Whether `path` is a path a file of a tree can have: a safe relative
/// path ending in `.jsonc`, or a chunk's, whose layer and coordinates may
/// be negative.
fn is_path(path: &str) -> bool {
    path.len() <= 1024 && (is_jsonc_path(path) || chunk_path(path).is_some())
}

/// `id` as an item of `kind`, when it is written as IDs of that kind are.
pub fn address<'a>(kind: ItemKind, id: &'a str) -> Option<Address<'a>> {
    let one = |make: fn(&'a str) -> Address<'a>| is_identifier(id).then(|| make(id));
    let path = |make: fn(&'a str) -> Address<'a>| is_jsonc_path(id).then(|| make(id));
    let file = |make: fn(&'a str) -> Address<'a>| is_path(id).then(|| make(id));
    match kind {
        ItemKind::Anchor => owned(id).map(|(plane, id)| Address::Anchor { plane, id }),
        ItemKind::Zone => owned(id).map(|(plane, id)| Address::Zone { plane, id }),
        ItemKind::Stairwell => owned(id).map(|(plane, id)| Address::Stairwell { plane, id }),
        ItemKind::Definition => owned(id).map(|(kind, id)| Address::Definition { kind, id }),
        ItemKind::DefinitionType => one(Address::DefinitionType),
        ItemKind::Plane => one(Address::Plane),
        ItemKind::RenderLayer => one(Address::RenderLayer),
        ItemKind::GroundLayer => one(Address::GroundLayer),
        ItemKind::Terrain => one(Address::Terrain),
        ItemKind::File => file(Address::File),
        ItemKind::Chunk => {
            (id.len() <= 1024 && chunk_path(id).is_some()).then_some(Address::Chunk(id))
        }
        ItemKind::Profile => path(Address::Profile),
        ItemKind::Schema => path(Address::Schema),
        ItemKind::Transitions => path(Address::Transitions),
        ItemKind::Manifest => (id == MANIFEST).then_some(Address::Manifest),
        ItemKind::Object => match id.split_once('#') {
            None => one(Address::Object),
            Some((chunk, ordinal)) => {
                // Digits only, without a leading zero, so one object has
                // one spelling.
                let digits = !ordinal.is_empty()
                    && ordinal.bytes().all(|byte| byte.is_ascii_digit())
                    && (ordinal.len() == 1 || !ordinal.starts_with('0'));
                let ordinal = digits.then(|| ordinal.parse().ok()).flatten()?;
                (chunk.len() <= 1024 && chunk_path(chunk).is_some())
                    .then_some(Address::ObjectAt { chunk, ordinal })
            }
        },
    }
}

/// The chunk whose path is `path`, with its plane and key: the path a
/// load gives it, `planes/<plane>/layers/<layer>/<x>_<y>.jsonc`.
fn chunk<'w>(world: &'w World, path: &str) -> Option<(&'w str, ChunkKey, &'w Chunk)> {
    let (plane, key) = chunk_path(path)?;
    let (plane, found) = world.planes().get_key_value(plane)?;
    let chunk = found.chunks.get(&key)?;
    // Another spelling of the same numbers is not the chunk's path.
    (chunk.path == path).then_some((plane.as_str(), key, chunk))
}

/// The profile document's path: what the manifest's `profile` names.
fn profile_path(index: &ReferenceIndex) -> Option<&str> {
    index
        .from(Holder::Manifest)
        .find(|reference| reference.role() == Role::ManifestProfile)
        .and_then(|reference| match reference.target() {
            Target::File(path) => Some(path),
            _ => None,
        })
}

/// An object found by its address: its plane, chunk key and ordinal.
fn object_ordinal<'w>(
    world: &'w World,
    address: Address<'_>,
) -> Option<(&'w str, ChunkKey, &'w Chunk, usize)> {
    match address {
        Address::Object(uid) => {
            let location = world.object_location(uid)?;
            let (plane, found) = world.planes().get_key_value(&location.plane)?;
            let chunk = found.chunks.get(&location.chunk)?;
            let ordinal =
                search::chunk_objects(chunk).position(|object| object.position == location.path)?;
            Some((plane.as_str(), location.chunk, chunk, ordinal))
        }
        Address::ObjectAt {
            chunk: path,
            ordinal,
        } => {
            let (plane, key, chunk) = chunk(world, path)?;
            (ordinal < search::chunk_objects(chunk).count()).then_some((plane, key, chunk, ordinal))
        }
        _ => None,
    }
}

/// What a reference to the item names, when references can name it.
pub fn target(address: Address<'_>) -> Option<Target<'_>> {
    Some(match address {
        Address::Definition { kind, id } => Target::Definition { kind, id },
        Address::DefinitionType(kind) => Target::DefinitionType(kind),
        Address::Plane(plane) => Target::Plane(plane),
        Address::Anchor { plane, id } => Target::Anchor { plane, id },
        Address::Zone { plane, id } => Target::Zone { plane, id },
        Address::RenderLayer(layer) => Target::RenderLayer(layer),
        Address::GroundLayer(layer) => Target::GroundLayer(layer),
        Address::Terrain(terrain) => Target::Terrain(terrain),
        Address::File(path) => Target::File(path),
        _ => return None,
    })
}

/// The item as a holder of references, when it exists and holds them.
fn holder<'a>(
    world: &'a World,
    index: &ReferenceIndex,
    address: Address<'a>,
) -> Option<Holder<'a>> {
    Some(match address {
        Address::Manifest => Holder::Manifest,
        Address::Profile(path) => (profile_path(index) == Some(path)).then_some(Holder::Profile)?,
        Address::Schema(path) => Holder::Schema { path },
        Address::Transitions(path) => world
            .profile()
            .transitions
            .as_ref()
            .is_some_and(|transitions| transitions.path() == path)
            .then_some(Holder::Transitions)?,
        Address::Definition { kind, id } => Holder::Definition { kind, id },
        Address::Plane(plane) => Holder::Plane { plane },
        Address::Zone { plane, id } => Holder::Zone { plane, zone: id },
        Address::Stairwell { plane, id } => Holder::Stairwell {
            plane,
            stairwell: id,
        },
        Address::Chunk(path) => {
            let (plane, chunk, _) = chunk(world, path)?;
            Holder::Chunk { plane, chunk }
        }
        Address::Object(_) | Address::ObjectAt { .. } => {
            let (plane, chunk, _, ordinal) = object_ordinal(world, address)?;
            Holder::Object {
                plane,
                chunk,
                ordinal,
            }
        }
        _ => return None,
    })
}

/// Whether the snapshot holds the item.
pub fn exists(world: &World, index: &ReferenceIndex, address: Address<'_>) -> bool {
    let profile = world.profile();
    let plane = |id: &str| world.planes().get(id);
    match address {
        Address::Anchor { plane: p, id } => plane(p).is_some_and(|p| p.anchors.contains_key(id)),
        Address::Zone { plane: p, id } => plane(p).is_some_and(|p| p.zones.contains_key(id)),
        Address::Stairwell { plane: p, id } => {
            plane(p).is_some_and(|p| p.stairwells.contains_key(id))
        }
        Address::Plane(p) => plane(p).is_some(),
        Address::Definition { kind, id } => world
            .definitions()
            .contains_key(&(kind.to_owned(), id.to_owned())),
        Address::DefinitionType(kind) => profile.definition_types.contains_key(kind),
        Address::RenderLayer(layer) => profile.render_layers.iter().any(|name| name == layer),
        Address::GroundLayer(layer) => profile.ground_layers.iter().any(|name| name == layer),
        Address::Terrain(terrain) => profile
            .transitions
            .as_ref()
            .is_some_and(|transitions| transitions.terrain(terrain).is_some()),
        Address::File(path) => world.files().contains_key(path),
        Address::Chunk(path) => chunk(world, path).is_some(),
        Address::Object(_) | Address::ObjectAt { .. } => object_ordinal(world, address).is_some(),
        Address::Manifest | Address::Profile(_) | Address::Transitions(_) => {
            holder(world, index, address).is_some()
        }
        // A schema the profile names, whether or not it holds references.
        Address::Schema(path) => index
            .to(Target::File(path))
            .any(|reference| reference.role() == Role::ProfileSchema),
    }
}

/// The names of the roles, in the index's order of roles; `other` stands
/// for a role a later library adds.
pub const ROLE_NAMES: [&str; 24] = [
    "manifest_profile",
    "manifest_plane",
    "profile_ground_layer",
    "profile_schema",
    "profile_transitions",
    "schema_target",
    "definition_type",
    "field",
    "zone_parent",
    "default_zone",
    "ground_palette",
    "zone_palette",
    "chunk_plane",
    "proto",
    "layer",
    "portal_plane",
    "portal_anchor",
    "stairwell_step",
    "terrain_layer",
    "terrain_proto",
    "transition_layer",
    "transition_blend",
    "transition_tile",
    "other",
];

/// The position of a role's name in [`ROLE_NAMES`].
pub fn role_index(role: Role<'_>) -> usize {
    match role {
        Role::ManifestProfile => 0,
        Role::ManifestPlane => 1,
        Role::ProfileGroundLayer => 2,
        Role::ProfileSchema => 3,
        Role::ProfileTransitions => 4,
        Role::SchemaTarget { .. } => 5,
        Role::DefinitionType => 6,
        Role::Field { .. } => 7,
        Role::ZoneParent => 8,
        Role::DefaultZone => 9,
        Role::GroundPalette { .. } => 10,
        Role::ZonePalette => 11,
        Role::ChunkPlane => 12,
        Role::Proto => 13,
        Role::Layer => 14,
        Role::PortalPlane => 15,
        Role::PortalAnchor { .. } => 16,
        Role::StairwellStep => 17,
        Role::TerrainLayer => 18,
        Role::TerrainProto => 19,
        Role::TransitionLayer => 20,
        Role::TransitionBlend => 21,
        Role::TransitionTile => 22,
        _ => 23,
    }
}

/// A set of roles, by their positions in [`ROLE_NAMES`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Roles(pub u32);

impl Roles {
    pub const ALL: Self = Self(u32::MAX);

    pub fn contains(self, role: Role<'_>) -> bool {
        self.0 & (1 << role_index(role)) != 0
    }
}

/// A target's kind and ID as `references` writes them.
fn target_id(target: Target<'_>) -> (&'static str, String) {
    match target {
        Target::Definition { kind, id } => ("definition", format!("{kind}/{id}")),
        Target::DefinitionType(kind) => ("definition_type", kind.to_owned()),
        Target::Plane(plane) => ("plane", plane.to_owned()),
        Target::Anchor { plane, id } => ("anchor", format!("{plane}/{id}")),
        Target::Zone { plane, id } => ("zone", format!("{plane}/{id}")),
        Target::RenderLayer(layer) => ("render_layer", layer.to_owned()),
        Target::GroundLayer(layer) => ("ground_layer", layer.to_owned()),
        Target::Terrain(terrain) => ("terrain", terrain.to_owned()),
        Target::File(path) => ("file", path.to_owned()),
        _ => ("other", String::new()),
    }
}

/// The facts about an object a reference item gives.
struct ObjectFacts<'w> {
    uid: Option<&'w str>,
    carried_by: Option<&'w str>,
    position: Vec<usize>,
}

/// Finds objects by chunk and ordinal for the items of one page. A
/// target's references come in source order, so the objects of one chunk
/// come with rising ordinals, and one walk of the chunk serves them all.
pub struct Objects<'w> {
    world: &'w World,
    walk: Option<Walk<'w>>,
}

struct Walk<'w> {
    plane: &'w str,
    chunk: ChunkKey,
    next: usize,
    objects: Box<dyn Iterator<Item = search::ChunkObject<'w>> + 'w>,
}

impl<'w> Objects<'w> {
    pub const fn new(world: &'w World) -> Self {
        Self { world, walk: None }
    }

    fn find(&mut self, plane: &str, key: ChunkKey, ordinal: usize) -> Option<ObjectFacts<'w>> {
        let reusable = self
            .walk
            .as_ref()
            .is_some_and(|walk| walk.plane == plane && walk.chunk == key && walk.next <= ordinal);
        if !reusable {
            let (plane, found) = self.world.planes().get_key_value(plane)?;
            let chunk = found.chunks.get(&key)?;
            self.walk = Some(Walk {
                plane,
                chunk: key,
                next: 0,
                objects: Box::new(search::chunk_objects(chunk)),
            });
        }
        let walk = self.walk.as_mut()?;
        let skip = ordinal - walk.next;
        let object = walk.objects.nth(skip)?;
        walk.next = ordinal + 1;
        Some(ObjectFacts {
            uid: object.uid,
            carried_by: object.carried_by,
            position: object.position,
        })
    }
}

/// `"null"` or `text` as a cut string.
fn nullable(output: &mut Output, text: Option<&str>) {
    match text {
        Some(text) => {
            output.string(text);
        }
        None => output.raw("null"),
    }
}

/// Writes the holder of `reference`.
fn write_holder(output: &mut Output, reference: &Reference<'_>, objects: &mut Objects<'_>) {
    let file = reference.file();
    let (kind, id, plane, chunk): (&str, Option<String>, Option<&str>, Option<&str>) =
        match reference.holder() {
            Holder::Manifest => ("manifest", Some(file.to_owned()), None, None),
            Holder::Profile => ("profile", Some(file.to_owned()), None, None),
            Holder::Schema { path } => ("schema", Some(path.to_owned()), None, None),
            Holder::Transitions => ("transitions", Some(file.to_owned()), None, None),
            Holder::Definition { kind, id } => {
                ("definition", Some(format!("{kind}/{id}")), None, None)
            }
            Holder::Plane { plane } => ("plane", Some(plane.to_owned()), Some(plane), None),
            Holder::Zone { plane, zone } => {
                ("zone", Some(format!("{plane}/{zone}")), Some(plane), None)
            }
            Holder::Stairwell { plane, stairwell } => (
                "stairwell",
                Some(format!("{plane}/{stairwell}")),
                Some(plane),
                None,
            ),
            Holder::Chunk { plane, .. } => {
                ("chunk", Some(file.to_owned()), Some(plane), Some(file))
            }
            Holder::Object {
                plane,
                chunk,
                ordinal,
            } => {
                let facts = objects.find(plane, chunk, ordinal);
                output.raw("{\"kind\":\"object\",\"id\":");
                nullable(output, facts.as_ref().and_then(|facts| facts.uid));
                output.raw(",\"plane\":");
                output.string(plane);
                output.raw(",\"chunk\":");
                output.string(file);
                let _ = write!(output.text, ",\"ordinal\":{ordinal},\"position\":");
                match &facts {
                    Some(facts) => {
                        let steps: Vec<String> =
                            facts.position.iter().map(ToString::to_string).collect();
                        let _ = write!(output.text, "[{}]", steps.join(","));
                    }
                    None => output.raw("null"),
                }
                output.raw(",\"carried_by\":");
                nullable(output, facts.as_ref().and_then(|facts| facts.carried_by));
                output.raw("}");
                return;
            }
            _ => ("other", None, None, None),
        };
    let _ = write!(output.text, "{{\"kind\":\"{kind}\",\"id\":");
    nullable(output, id.as_deref());
    output.raw(",\"plane\":");
    nullable(output, plane);
    output.raw(",\"chunk\":");
    nullable(output, chunk);
    output.raw(",\"ordinal\":null,\"position\":null,\"carried_by\":null}");
}

/// Writes one reference item: its role, what holds it, what it names,
/// and where it is written.
pub fn write_reference(output: &mut Output, reference: &Reference<'_>, objects: &mut Objects<'_>) {
    let role = reference.role();
    let _ = write!(
        output.text,
        "{{\"role\":\"{}\",\"field\":",
        ROLE_NAMES[role_index(role)]
    );
    let (field, layer, implicit) = match role {
        Role::SchemaTarget { field } | Role::Field { field } => (Some(field), None, None),
        Role::GroundPalette { layer } => (None, Some(layer), None),
        Role::PortalAnchor { implicit_plane } => (None, None, Some(implicit_plane)),
        _ => (None, None, None),
    };
    nullable(output, field);
    output.raw(",\"layer\":");
    nullable(output, layer);
    output.raw(",\"implicit_plane\":");
    output.raw(match implicit {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    });
    output.raw(",\"from\":");
    write_holder(output, reference, objects);
    let (kind, id) = target_id(reference.target());
    let _ = write!(output.text, ",\"to\":{{\"kind\":\"{kind}\",\"id\":");
    output.string(&id);
    output.raw("},\"path\":");
    output.string(reference.file());
    let span = reference.span();
    let _ = write!(
        output.text,
        ",\"start\":{},\"end\":{}}}",
        span.start, span.end
    );
}

/// Which way `references` looks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// The references that name the item.
    To,
    /// The references the item holds.
    From,
}

/// The references to or from the item at `address`, in the index's
/// order: by source order, a binary search each way.
pub fn lookup<'i>(
    world: &'i World,
    index: &'i ReferenceIndex,
    address: Address<'i>,
    direction: Direction,
) -> (bool, Box<dyn ExactSizeIterator<Item = Reference<'i>> + 'i>) {
    let exists = exists(world, index, address);
    let found: Box<dyn ExactSizeIterator<Item = Reference<'i>> + 'i> = match direction {
        Direction::To => match target(address) {
            Some(target) => Box::new(index.to(target)),
            None => Box::new(std::iter::empty()),
        },
        Direction::From => match holder(world, index, address) {
            Some(holder) => Box::new(index.from(holder)),
            None => Box::new(std::iter::empty()),
        },
    };
    (exists, found)
}

/// What `impact` is asked about.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action<'a> {
    Remove,
    /// A rename to this item, of the same kind and scope.
    Rename(Address<'a>),
}

/// Whether the reference's holder is part of the item at `address`, so
/// that it goes with the item when the item is removed.
fn held_by(address: Address<'_>, holder: Holder<'_>) -> bool {
    match (address, holder) {
        (Address::Definition { kind, id }, Holder::Definition { kind: k, id: i }) => {
            kind == k && id == i
        }
        (
            Address::Plane(plane),
            Holder::Plane { plane: p }
            | Holder::Zone { plane: p, .. }
            | Holder::Stairwell { plane: p, .. }
            | Holder::Chunk { plane: p, .. }
            | Holder::Object { plane: p, .. },
        ) => plane == p,
        (Address::Zone { plane, id }, Holder::Zone { plane: p, zone }) => plane == p && id == zone,
        _ => false,
    }
}

/// The references that break when `action` is taken on the item: every
/// reference to it, but on removal not those it holds itself, which go
/// with it; for a plane, also the references from outside it to its
/// anchors when it is removed. Zones are named only from their own
/// plane, so only from what goes with it. Renaming a plane breaks the
/// references to it, which name it, and not those to its zones and
/// anchors, whose plane is implicit in what is written or written as a
/// portal's plane, which names the plane. In the index's order: the
/// item's references, then each anchor's by ID.
pub fn breaking<'i>(
    world: &'i World,
    index: &'i ReferenceIndex,
    address: Address<'i>,
    action: Action<'_>,
) -> Box<dyn Iterator<Item = Reference<'i>> + 'i> {
    let Some(target) = target(address) else {
        return Box::new(std::iter::empty());
    };
    let removed = action == Action::Remove;
    let item = index
        .to(target)
        .filter(move |reference| !(removed && held_by(address, reference.holder())));
    match (address, removed) {
        (Address::Plane(plane), true) => {
            let anchors = world
                .planes()
                .get(plane)
                .map(|found| found.anchors.keys())
                .into_iter()
                .flatten()
                .flat_map(move |id| {
                    index
                        .to(Target::Anchor { plane, id })
                        .filter(move |reference| !held_by(address, reference.holder()))
                });
            Box::new(item.chain(anchors))
        }
        _ => Box::new(item),
    }
}

/// What a plane's removal or rename takes with it.
pub struct Cascade {
    pub zones: usize,
    pub anchors: usize,
    pub stairwells: usize,
    pub chunks: usize,
    pub objects: usize,
    /// References from outside the plane to its anchors and zones.
    pub incoming: usize,
    /// Portals of the plane that omit their plane and name one of its
    /// anchors: they lead within the plane whatever it is called.
    pub implicit: usize,
}

pub fn cascade(world: &World, index: &ReferenceIndex, plane: &str, objects: usize) -> Cascade {
    let Some(found) = world.planes().get(plane) else {
        return Cascade {
            zones: 0,
            anchors: 0,
            stairwells: 0,
            chunks: 0,
            objects: 0,
            incoming: 0,
            implicit: 0,
        };
    };
    let address = Address::Plane(plane);
    let mut incoming = 0;
    let mut implicit = 0;
    for id in found.anchors.keys() {
        for reference in index.to(Target::Anchor { plane, id }) {
            if !held_by(address, reference.holder()) {
                incoming += 1;
            } else if reference.role()
                == (Role::PortalAnchor {
                    implicit_plane: true,
                })
            {
                implicit += 1;
            }
        }
    }
    for id in found.zones.keys() {
        incoming += index
            .to(Target::Zone { plane, id })
            .filter(|reference| !held_by(address, reference.holder()))
            .count();
    }
    Cascade {
        zones: found.zones.len(),
        anchors: found.anchors.len(),
        stairwells: found.stairwells.len(),
        chunks: found.chunks.len(),
        objects,
        incoming,
        implicit,
    }
}

/// Most of the item's own files an `impact` result lists.
pub const MAXIMUM_OWN_FILES: usize = 100;

/// The files of the snapshot that hold the item itself, which a removal
/// deletes or edits and a rename moves or edits: the definition's file, a
/// plane's directory, the file that holds an entry, an object's chunk, the
/// profile for a render layer and the transitions document for a terrain.
pub fn own_files<'w>(
    world: &'w World,
    index: &'w ReferenceIndex,
    address: Address<'_>,
) -> Box<dyn Iterator<Item = &'w str> + 'w> {
    let file = |path: String| -> Box<dyn Iterator<Item = &'w str> + 'w> {
        Box::new(
            world
                .files()
                .get_key_value(path.as_str())
                .map(|(path, _)| path.as_str())
                .into_iter(),
        )
    };
    match address {
        Address::Definition { kind, id } => file(format!("defs/{kind}/{id}.jsonc")),
        Address::Plane(plane) => {
            let prefix = format!("planes/{plane}/");
            Box::new(
                world
                    .files()
                    .range(prefix.clone()..)
                    .map(|(path, _)| path.as_str())
                    .take_while(move |path| path.starts_with(&prefix)),
            )
        }
        Address::Zone { plane, .. } => file(format!("planes/{plane}/zones.jsonc")),
        Address::Anchor { plane, .. } => file(format!("planes/{plane}/anchors.jsonc")),
        Address::Stairwell { plane, .. } => file(format!("planes/{plane}/stairwells.jsonc")),
        Address::Object(uid) => match world.object_location(uid) {
            Some(location) => match world
                .planes()
                .get(&location.plane)
                .and_then(|plane| plane.chunks.get(&location.chunk))
            {
                Some(chunk) => file(chunk.path.clone()),
                None => Box::new(std::iter::empty()),
            },
            None => Box::new(std::iter::empty()),
        },
        Address::RenderLayer(_) => match profile_path(index) {
            Some(path) => file(path.to_owned()),
            None => Box::new(std::iter::empty()),
        },
        Address::Terrain(_) => match &world.profile().transitions {
            Some(transitions) => file(transitions.path().to_owned()),
            None => Box::new(std::iter::empty()),
        },
        _ => Box::new(std::iter::empty()),
    }
}

/// The kind `tilewright diff` names the item's IDs with, when it has one.
pub fn diff_kind(kind: ItemKind) -> Option<tilewright_diff::Kind> {
    use tilewright_diff::Kind;
    Some(match kind {
        ItemKind::Anchor => Kind::Anchor,
        ItemKind::Definition => Kind::Definition,
        ItemKind::Object => Kind::Object,
        ItemKind::Plane => Kind::Plane,
        ItemKind::RenderLayer => Kind::RenderLayer,
        ItemKind::Stairwell => Kind::Stairwell,
        ItemKind::Zone => Kind::Zone,
        _ => return None,
    })
}

/// What `impact` says it does not see, the same for every item.
pub const NOT_COVERED: &str = "Only the references the world format defines are covered: names written inside json field values and custom objects, and anything game code, saves or scripts keep, are not seen; nothing in the format references a stairwell or an object uid, so none breaks here, though a game's own data may keep them.";

/// Writes the page of `references` from `offset` within `budget`, and
/// returns it.
pub fn page<'i>(
    world: &'i World,
    references: impl Iterator<Item = Reference<'i>>,
    offset: usize,
    limit: usize,
    budget: usize,
) -> budget::Page {
    let mut objects = Objects::new(world);
    budget::page_within(references, offset, limit, budget, |output, reference| {
        write_reference(output, &reference, &mut objects);
    })
}

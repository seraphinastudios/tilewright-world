// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! What a comparison reads of one world: every ID, with a fingerprint of the
//! content that the rename rule of `docs/spec/DIFF.md` compares.

use crate::{Error, Kind, Limits, is_identifier};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    ops::Bound,
};
use tilewright_jsonc::{Document, Limits as ParseLimits, write_json_string};
use tilewright_pack::{Identity, Pack};
use tilewright_source::World;

/// SHA-256 of the content an ID's rename rule compares.
pub(crate) type Fingerprint = [u8; 32];

/// What a side of a comparison was read from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Input {
    Source,
    Pack,
}

impl Input {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Pack => "pack",
        }
    }
}

/// Where a snapshot came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Side {
    pub input: Input,
    /// The profile's `id`.
    pub profile: String,
    /// The profile's `version`.
    pub profile_version: String,
    /// Lowercase hex SHA-256 over the source files, as `docs/spec/PACK.md`
    /// defines it, so a source tree and the pack compiled from it agree.
    pub source_digest: String,
}

/// The IDs of one world, each with a fingerprint of its content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub(crate) side: Side,
    pub(crate) ids: BTreeMap<Kind, BTreeMap<String, Fingerprint>>,
    /// The plane each object uid's object is on, so that removing a plane
    /// can accept the removal of its objects' uids.
    pub(crate) object_planes: BTreeMap<String, String>,
}

impl Snapshot {
    /// The snapshot of a loaded source tree. The world is already in
    /// memory, so `limits` bound the snapshot built from it.
    ///
    /// # Errors
    /// Returns [`Error::TooManyIds`] if the world has more IDs than
    /// `limits` allow, or [`Error::Invalid`] if a document of the world
    /// cannot be read again, which a world that loaded never has.
    pub fn from_world(world: &World, limits: &Limits) -> Result<Self, Error> {
        let profile = world.profile();
        let mut builder = Builder::new(*limits);
        let mut read = || -> Result<(), String> {
            for (position, name) in profile.render_layers.iter().enumerate() {
                builder.render_layer(name, position)?;
            }
            for (id, plane) in world.planes() {
                builder.plane(id, &plane.canonical)?;
                for (zone, entry) in &plane.zones {
                    builder.zone(id, zone, &entry.canonical)?;
                }
                for (anchor, entry) in &plane.anchors {
                    builder.anchor(id, anchor, entry.layer, entry.at)?;
                }
                for (stairwell, entry) in &plane.stairwells {
                    builder.stairwell(id, stairwell, &entry.canonical)?;
                }
                for (&(layer, cx, cy), chunk) in &plane.chunks {
                    builder.chunk(id, layer, cx, cy)?;
                    // Depth-first order is a compiled record's, so an
                    // object's index is the one its pack row holds.
                    for (index, object) in chunk.depth_first().enumerate() {
                        if let Some(uid) = &object.uid {
                            builder.object(uid, id, layer, cx, cy, index)?;
                        }
                    }
                }
            }
            for ((kind, id), definition) in world.definitions() {
                builder.definition(kind, id, &definition.canonical)?;
            }
            Ok(())
        };
        let read = read();
        builder.check()?;
        read.map_err(Error::Invalid)?;
        Ok(builder.finish(Side {
            input: Input::Source,
            profile: profile.id.clone(),
            profile_version: profile.version.clone(),
            source_digest: hex(&world.source_digest()),
        }))
    }

    /// The snapshot of a content pack. The pack is fully verified first,
    /// as [`Pack::verify`] does, and its IDs are then read through
    /// [`Pack::visit_identities`] one row at a time; reading stops as soon
    /// as the pack has more IDs than `limits` allow.
    ///
    /// # Errors
    /// Returns [`Error::TooManyIds`] past the limit, or [`Error::Pack`] if
    /// the pack does not verify or SQLite fails.
    pub fn from_pack(pack: &Pack, limits: &Limits) -> Result<Self, Error> {
        let mut builder = Builder::new(*limits);
        // The visit checks the stored profile before anything else, and
        // with it the profile ID and version read below.
        let read = pack.visit_identities(&mut |identity| {
            match identity {
                Identity::RenderLayer { name, position } => builder.render_layer(name, position),
                Identity::Plane { id, document } => builder.plane(id, document),
                Identity::Zone {
                    plane,
                    id,
                    document,
                } => builder.zone(plane, id, document),
                Identity::Anchor {
                    plane,
                    id,
                    layer,
                    at,
                } => builder.anchor(plane, id, layer, at),
                Identity::Stairwell {
                    plane,
                    id,
                    document,
                } => builder.stairwell(plane, id, document),
                Identity::Definition { kind, id, document } => {
                    builder.definition(kind, id, document)
                }
                Identity::Chunk {
                    plane,
                    layer,
                    cx,
                    cy,
                } => builder.chunk(plane, layer, cx, cy),
                Identity::Object {
                    uid,
                    plane,
                    layer,
                    cx,
                    cy,
                    index,
                } => builder.object(uid, plane, layer, cx, cy, index),
            }
            .map_err(tilewright_pack::Error::Invalid)
        });
        builder.check()?;
        read.map_err(Error::Pack)?;
        let meta = |key: &str| -> Result<String, Error> {
            pack.meta(key).map_err(Error::Pack)?.ok_or_else(|| {
                Error::Pack(tilewright_pack::Error::Invalid(format!(
                    "the pack has no {key}"
                )))
            })
        };
        Ok(builder.finish(Side {
            input: Input::Pack,
            profile: meta("profile_id")?,
            profile_version: meta("profile_version")?,
            source_digest: meta("source_digest")?,
        }))
    }

    /// Where the snapshot came from.
    #[must_use]
    pub const fn side(&self) -> &Side {
        &self.side
    }

    /// Whether the world has ID `id` of `kind`.
    #[must_use]
    pub fn contains(&self, kind: Kind, id: &str) -> bool {
        self.ids.get(&kind).is_some_and(|ids| ids.contains_key(id))
    }

    /// The plane the object with `uid` is on.
    #[must_use]
    pub fn object_plane(&self, uid: &str) -> Option<&str> {
        self.object_planes.get(uid).map(String::as_str)
    }

    /// The IDs of `kind`, in byte order.
    pub fn ids(&self, kind: Kind) -> impl Iterator<Item = &str> {
        self.ids
            .get(&kind)
            .into_iter()
            .flat_map(|ids| ids.keys().map(String::as_str))
    }

    /// The IDs of `kind` with their fingerprints, in byte order.
    pub(crate) fn entries(&self, kind: Kind) -> impl Iterator<Item = (&str, &Fingerprint)> {
        self.ids.get(&kind).into_iter().flat_map(|ids| {
            ids.iter()
                .map(|(id, fingerprint)| (id.as_str(), fingerprint))
        })
    }
}

/// Collects a snapshot. A plane's fingerprint covers its zones, anchors and
/// stairwells, so it is finished once everything has been read.
struct Builder {
    ids: BTreeMap<Kind, BTreeMap<String, Fingerprint>>,
    /// Each plane's document fingerprint and a hash of its chunk addresses,
    /// fed in `(layer, cx, cy)` order.
    planes: BTreeMap<String, (Fingerprint, Sha256)>,
    /// The plane of each object uid.
    object_planes: BTreeMap<String, String>,
    /// The most IDs the snapshot may hold, and how many it holds.
    maximum: usize,
    count: usize,
}

impl Builder {
    const fn new(limits: Limits) -> Self {
        Self {
            ids: BTreeMap::new(),
            planes: BTreeMap::new(),
            object_planes: BTreeMap::new(),
            maximum: limits.maximum_ids,
            count: 0,
        }
    }

    /// Counts one more ID before anything of it is kept, failing once the
    /// count passes the limit.
    fn admit(&mut self) -> Result<(), String> {
        self.count = self.count.saturating_add(1);
        if self.count > self.maximum {
            return Err(format!("more than {} IDs", self.maximum));
        }
        Ok(())
    }

    /// [`Error::TooManyIds`] if reading stopped at the limit.
    const fn check(&self) -> Result<(), Error> {
        if self.count > self.maximum {
            Err(Error::TooManyIds(self.maximum))
        } else {
            Ok(())
        }
    }

    fn insert(&mut self, kind: Kind, id: String, fingerprint: Fingerprint) -> Result<(), String> {
        let ids = self.ids.entry(kind).or_default();
        if ids.contains_key(&id) {
            return Err(format!("{kind} {id} is listed twice"));
        }
        ids.insert(id, fingerprint);
        Ok(())
    }

    /// A render layer stands in for its content with its position in the
    /// profile's list.
    fn render_layer(&mut self, name: &str, position: usize) -> Result<(), String> {
        self.admit()?;
        if !is_identifier(name) {
            return Err("a render layer is not an identifier".into());
        }
        let fingerprint = Sha256::digest(position.to_string()).into();
        self.insert(Kind::RenderLayer, name.to_owned(), fingerprint)
    }

    fn plane(&mut self, id: &str, document: &str) -> Result<(), String> {
        self.admit()?;
        let fingerprint = document_fingerprint(document, &["id", "title"])?;
        if self
            .planes
            .insert(id.to_owned(), (fingerprint, Sha256::new()))
            .is_some()
        {
            return Err(format!("plane {id} is listed twice"));
        }
        Ok(())
    }

    fn zone(&mut self, plane: &str, id: &str, document: &str) -> Result<(), String> {
        self.admit()?;
        let fingerprint = document_fingerprint(document, &["id", "name"])?;
        self.insert(Kind::Zone, format!("{plane}/{id}"), fingerprint)
    }

    fn anchor(&mut self, plane: &str, id: &str, layer: i32, at: [i64; 2]) -> Result<(), String> {
        self.admit()?;
        let content = format!("{{\"layer\":{layer},\"at\":[{},{}]}}", at[0], at[1]);
        self.insert(
            Kind::Anchor,
            format!("{plane}/{id}"),
            Sha256::digest(content).into(),
        )
    }

    fn stairwell(&mut self, plane: &str, id: &str, document: &str) -> Result<(), String> {
        self.admit()?;
        let fingerprint = document_fingerprint(document, &["id"])?;
        self.insert(Kind::Stairwell, format!("{plane}/{id}"), fingerprint)
    }

    fn definition(&mut self, kind: &str, id: &str, document: &str) -> Result<(), String> {
        self.admit()?;
        let fingerprint = document_fingerprint(document, &["id"])?;
        self.insert(Kind::Definition, format!("{kind}/{id}"), fingerprint)
    }

    /// An object stands in for its content with its place: its chunk's
    /// plane, layer and coordinates, and its index in the chunk's
    /// depth-first order.
    fn object(
        &mut self,
        uid: &str,
        plane: &str,
        layer: i32,
        cx: i64,
        cy: i64,
        index: usize,
    ) -> Result<(), String> {
        self.admit()?;
        if !is_identifier(uid) {
            return Err("an object uid is not an identifier".into());
        }
        let mut content = String::from("{\"plane\":");
        write_json_string(plane, &mut content);
        let _ = write!(
            content,
            ",\"layer\":{layer},\"chunk\":[{cx},{cy}],\"index\":{index}}}"
        );
        self.insert(Kind::Object, uid.to_owned(), Sha256::digest(content).into())?;
        self.object_planes.insert(uid.to_owned(), plane.to_owned());
        Ok(())
    }

    fn chunk(&mut self, plane: &str, layer: i32, cx: i64, cy: i64) -> Result<(), String> {
        let (_, chunks) = self
            .planes
            .get_mut(plane)
            .ok_or_else(|| format!("a chunk names unknown plane {plane}"))?;
        chunks.update(layer.to_le_bytes());
        chunks.update(cx.to_le_bytes());
        chunks.update(cy.to_le_bytes());
        Ok(())
    }

    fn finish(self, side: Side) -> Snapshot {
        let Self {
            mut ids,
            planes,
            object_planes,
            ..
        } = self;
        let mut fingerprints = BTreeMap::new();
        for (plane, (document, chunks)) in planes {
            let mut hasher = Sha256::new();
            hasher.update(document);
            hasher.update(chunks.finalize());
            let prefix = format!("{plane}/");
            for kind in [Kind::Anchor, Kind::Stairwell, Kind::Zone] {
                let Some(children) = ids.get(&kind) else {
                    continue;
                };
                let range = children
                    .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
                    .take_while(|(id, _)| id.starts_with(&prefix));
                for (id, fingerprint) in range {
                    feed(&mut hasher, kind.as_str().as_bytes());
                    feed(&mut hasher, &id.as_bytes()[prefix.len()..]);
                    hasher.update(fingerprint);
                }
            }
            fingerprints.insert(plane, hasher.finalize().into());
        }
        ids.insert(Kind::Plane, fingerprints);
        Snapshot {
            side,
            ids,
            object_planes,
        }
    }
}

/// Feeds `bytes` to `hasher`, prefixed with their length, so consecutive
/// fields cannot run together.
fn feed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

/// Limits for reading back canonical JSON, which is one line: the format's
/// limits, with the line limit raised to the input limit.
fn canonical_limits() -> ParseLimits {
    let limits = ParseLimits::default();
    ParseLimits {
        max_line_bytes: limits.max_input_bytes,
        ..limits
    }
}

/// SHA-256 of a canonical JSON object without the members named `without`,
/// as canonical JSON.
fn document_fingerprint(document: &str, without: &[&str]) -> Result<Fingerprint, String> {
    let parsed = Document::parse(document, &canonical_limits())
        .map_err(|error| format!("a stored document cannot be read again: {error}"))?;
    let members = parsed
        .root()
        .as_object()
        .ok_or("a stored document is not an object")?;
    let excluded: BTreeSet<&str> = without.iter().copied().collect();
    let mut hasher = Sha256::new();
    hasher.update(b"{");
    let mut first = true;
    let mut key = String::new();
    for member in members
        .iter()
        .filter(|member| !excluded.contains(member.key.as_str()))
    {
        if !first {
            hasher.update(b",");
        }
        first = false;
        key.clear();
        write_json_string(&member.key, &mut key);
        hasher.update(key.as_bytes());
        hasher.update(b":");
        hasher.update(parsed.canonical(&member.value).as_bytes());
    }
    hasher.update(b"}");
    Ok(hasher.finalize().into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"context"
	"database/sql"
	"fmt"
	"math"
	"unicode/utf8"
)

// Projection is a profile's projection: isometric with TileWidth and
// TileHeight, or top_down with TileSize.
type Projection struct {
	Kind       string
	TileWidth  uint32
	TileHeight uint32
	TileSize   uint32
}

// Profile is the stored game profile, checked as a full verification checks
// it.
type Profile struct {
	ID      string
	Version string
	// Projection is the profile's projection.
	Projection Projection
	// RenderLayers are the draw layers in order; an object's Layer is an
	// index into them plus one.
	RenderLayers []string
	// GroundLayers are the layers of a chunk's Ground grids, in order.
	GroundLayers []string
	// LayerHeight is the height of one layer in height units, or 0 when
	// the profile does not stack layers.
	LayerHeight uint32
	// Document is the profile document as canonical JSON.
	Document string
}

// Profile returns the stored profile after checking all of it: canonical
// JSON that agrees with profile_id and profile_version, a projection and
// layer_height a source profile can have, and 1 to 65,534 unique render
// layers and at most 8 unique ground layers, each a render layer other
// than height or zone.
func (p *Pack) Profile(ctx context.Context) (Profile, error) {
	value, _, err := p.Meta(ctx, "profile")
	if err != nil {
		return Profile{}, err
	}
	id, _, err := p.Meta(ctx, "profile_id")
	if err != nil {
		return Profile{}, err
	}
	version, _, err := p.Meta(ctx, "profile_version")
	if err != nil {
		return Profile{}, err
	}
	return storedProfile(value, id, version)
}

// LayerHeight returns the profile's layer_height, and false when the
// profile does not stack layers. The stored profile is checked as Profile
// checks it.
func (p *Pack) LayerHeight(ctx context.Context) (uint32, bool, error) {
	profile, err := p.Profile(ctx)
	if err != nil {
		return 0, false, err
	}
	return profile.LayerHeight, profile.LayerHeight != 0, nil
}

func storedProfile(document, profileID, profileVersion string) (Profile, error) {
	root, err := storedDocument(sql.NullString{String: document, Valid: true}, CodeProfile, "the stored profile")
	if err != nil {
		return Profile{}, err
	}
	// The identity must be one a source profile can have: an identifier
	// and a version of at most 64 characters.
	format, _ := root.str("format")
	id, idOK := root.str("id")
	version, versionOK := root.str("version")
	if format != "tilewright.profile/1" || !idOK || !isIdentifier(id) ||
		!versionOK || utf8.RuneCountInString(version) > maxProfileVersion ||
		id != profileID || version != profileVersion {
		return Profile{}, reject(CodeProfile, "the stored profile disagrees with profile_id or profile_version")
	}
	projection, ok := storedProjection(&root)
	if !ok {
		return Profile{}, reject(CodeProfile, "the stored profile's projection is not one a source profile can have")
	}
	profile := Profile{ID: id, Version: version, Projection: projection, Document: document}
	if height, ok := root.get("layer_height"); ok {
		if height.kind != jsonInteger || height.integer < 1 || height.integer > maxLayerHeight {
			return Profile{}, reject(CodeProfile, fmt.Sprintf("the stored profile's layer_height is not an integer from 1 to %d", maxLayerHeight))
		}
		profile.LayerHeight = uint32(height.integer)
	}
	if profile.RenderLayers, ok = layerList(&root, "render_layers", 1, maxRenderLayers); !ok {
		return Profile{}, reject(CodeProfile, fmt.Sprintf("stored profile does not list 1..=%d unique render_layers", maxRenderLayers))
	}
	if profile.GroundLayers, ok = layerList(&root, "ground_layers", 0, maxGroundLayers); !ok {
		return Profile{}, reject(CodeProfile, fmt.Sprintf("stored profile does not list 0..=%d unique ground_layers", maxGroundLayers))
	}
	render := make(map[string]struct{}, len(profile.RenderLayers))
	for _, name := range profile.RenderLayers {
		render[name] = struct{}{}
	}
	for _, name := range profile.GroundLayers {
		if _, ok := render[name]; !ok || name == "height" || name == "zone" {
			return Profile{}, reject(CodeProfile, "a stored ground layer is not a render layer other than height or zone")
		}
	}
	return profile, nil
}

// storedProjection reads a projection a source profile can have:
// isometric with tile_width and tile_height, or top_down with tile_size,
// each an integer from 1 to 4,294,967,295, and no other keys.
func storedProjection(root *jsonValue) (Projection, bool) {
	value, ok := root.get("projection")
	if !ok || value.kind != jsonObject {
		return Projection{}, false
	}
	kind, _ := value.str("kind")
	var sizes []string
	switch kind {
	case "isometric":
		sizes = []string{"tile_width", "tile_height"}
	case "top_down":
		sizes = []string{"tile_size"}
	default:
		return Projection{}, false
	}
	if len(value.children) != len(sizes)+1 {
		return Projection{}, false
	}
	read := make([]uint32, len(sizes))
	for i, key := range sizes {
		size, ok := value.get(key)
		if !ok || size.kind != jsonInteger || size.integer < 1 || size.integer > math.MaxUint32 {
			return Projection{}, false
		}
		read[i] = uint32(size.integer)
	}
	projection := Projection{Kind: kind}
	if kind == "isometric" {
		projection.TileWidth, projection.TileHeight = read[0], read[1]
	} else {
		projection.TileSize = read[0]
	}
	return projection, true
}

// layerList reads a list of minimum to maximum unique identifiers.
func layerList(root *jsonValue, key string, minimum, maximum int) ([]string, bool) {
	value, ok := root.get(key)
	if !ok || value.kind != jsonArray || len(value.children) < minimum || len(value.children) > maximum {
		return nil, false
	}
	names := make([]string, 0, len(value.children))
	seen := make(map[string]struct{}, len(value.children))
	for _, item := range value.children {
		if item.kind != jsonString || !isIdentifier(item.text) {
			return nil, false
		}
		if _, duplicate := seen[item.text]; duplicate {
			return nil, false
		}
		seen[item.text] = struct{}{}
		names = append(names, item.text)
	}
	return names, true
}

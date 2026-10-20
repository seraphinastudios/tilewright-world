// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"context"
	"database/sql"
	"errors"
	"iter"
	"math"
)

// Direction is which way a stairwell may be used.
type Direction string

// Stairwell directions.
const (
	// DirectionBoth: from either end to the other.
	DirectionBoth Direction = "both"
	// DirectionDown: only from the upper end to the lower end.
	DirectionDown Direction = "down"
	// DirectionUp: only from the lower end to the upper end.
	DirectionUp Direction = "up"
)

// StairwellStep is one step of a stairwell, in plane coordinates.
type StairwellStep struct {
	Layer int32
	X, Y  int64
	Proto string
}

// Stairwell is one exit that spans several layers (docs/spec/FORMAT.md
// section 10), read from its stored document.
type Stairwell struct {
	ID string
	// Steps are connected: on adjacent layers in one direction, each at
	// most one tile from the one before it on each axis.
	Steps               []StairwellStep
	Direction           Direction
	HiddenOnOtherLayers bool
	// Custom is canonical JSON of the stairwell's custom data, or "" when
	// it has none.
	Custom string
	// Document is the stairwell's source entry as canonical JSON.
	Document string
}

// StairwellTile is the stairwell step on a tile.
type StairwellTile struct {
	// Stairwell is the stairwell's ID.
	Stairwell string
	// Step is the step's position in the stairwell's steps.
	Step  int
	Proto string
}

// closed reports whether value is an object whose keys are all in keys.
func closed(value *jsonValue, keys ...string) bool {
	if value.kind != jsonObject {
		return false
	}
	for _, member := range value.children {
		known := false
		for _, key := range keys {
			known = known || member.key == key
		}
		if !known {
			return false
		}
	}
	return true
}

// connects reports whether steps are at least two, on adjacent layers in
// one direction, each at most one tile from the one before it on each
// axis.
func connects(steps []StairwellStep) bool {
	if len(steps) < 2 {
		return false
	}
	change := int64(-1)
	if steps[1].Layer > steps[0].Layer {
		change = 1
	}
	for i := 1; i < len(steps); i++ {
		before, next := steps[i-1], steps[i]
		// Positions are within ±MaxSafeInteger, so these differences fit.
		if int64(next.Layer)-int64(before.Layer) != change ||
			next.X-before.X > 1 || before.X-next.X > 1 ||
			next.Y-before.Y > 1 || before.Y-next.Y > 1 {
			return false
		}
	}
	return true
}

// storedStairwell reads a stored stairwell document, already checked to be
// canonical JSON of an object with namespaced custom data, into a
// stairwell with the source format's shape: an identifier, at least two
// connected steps, each a 32-bit layer, a position within the integer
// range and a prototype identifier, and a valid direction and flag.
func storedStairwell(root *jsonValue, document string) (Stairwell, error) {
	invalid := reject(CodeStairwell, "a stairwell document does not have the source format's shape")
	if !closed(root, "id", "steps", "direction", "hidden_on_other_layers", "custom") {
		return Stairwell{}, invalid
	}
	id, ok := root.str("id")
	if !ok || !isIdentifier(id) {
		return Stairwell{}, invalid
	}
	stairwell := Stairwell{ID: id, Direction: DirectionBoth, HiddenOnOtherLayers: true, Document: document}
	if direction, ok := root.get("direction"); ok {
		if direction.kind != jsonString {
			return Stairwell{}, invalid
		}
		switch Direction(direction.text) {
		case DirectionBoth, DirectionDown, DirectionUp:
			stairwell.Direction = Direction(direction.text)
		default:
			return Stairwell{}, invalid
		}
	}
	if hidden, ok := root.get("hidden_on_other_layers"); ok {
		if hidden.kind != jsonBool {
			return Stairwell{}, invalid
		}
		stairwell.HiddenOnOtherLayers = hidden.boolean
	}
	steps, ok := root.get("steps")
	if !ok || steps.kind != jsonArray {
		return Stairwell{}, invalid
	}
	for i := range steps.children {
		step := &steps.children[i]
		if !closed(step, "layer", "at", "proto") {
			return Stairwell{}, invalid
		}
		layer, layerOK := step.get("layer")
		at, atOK := step.get("at")
		proto, protoOK := step.str("proto")
		if !layerOK || layer.kind != jsonInteger || layer.integer < math.MinInt32 || layer.integer > math.MaxInt32 ||
			!atOK || at.kind != jsonArray || len(at.children) != 2 ||
			at.children[0].kind != jsonInteger || at.children[1].kind != jsonInteger ||
			!protoOK || !isIdentifier(proto) {
			return Stairwell{}, invalid
		}
		stairwell.Steps = append(stairwell.Steps, StairwellStep{
			Layer: int32(layer.integer),
			X:     at.children[0].integer,
			Y:     at.children[1].integer,
			Proto: proto,
		})
	}
	if !connects(stairwell.Steps) {
		return Stairwell{}, reject(CodeStairwell, "the steps of stairwell "+id+" are not on adjacent layers in one direction, at most one tile apart")
	}
	if custom, ok := root.get("custom"); ok {
		stairwell.Custom = document[custom.start:custom.end]
	}
	return stairwell, nil
}

func stairwellSQL(where string) string {
	return "SELECT " + boundedIdentifier("id") + ", " + boundedText("document", "?1") + " FROM stairwells " + where
}

// scanStairwell reads a stairwell row of id and document, bounded in SQL,
// and checks that the document has the row's ID.
func scanStairwell(rows interface{ Scan(...any) error }) (Stairwell, error) {
	var id, document sql.NullString
	if err := rows.Scan(&id, &document); err != nil {
		return Stairwell{}, err
	}
	id, err := nullableText(id, CodeStairwell)
	if err != nil {
		return Stairwell{}, err
	}
	document, err = nullableText(document, CodeStairwell)
	if err != nil {
		return Stairwell{}, err
	}
	root, err := storedDocument(document, CodeStairwell, "a stairwell document")
	if err != nil {
		return Stairwell{}, err
	}
	stairwell, err := storedStairwell(&root, document.String)
	if err != nil {
		return Stairwell{}, err
	}
	if !id.Valid || id.String != stairwell.ID {
		return Stairwell{}, reject(CodeStairwell, "the document of stairwell "+stairwell.ID+" disagrees with its row")
	}
	return stairwell, nil
}

// Stairwell returns one stairwell of a plane, read from its stored
// document, and whether the pack has it. Like Chunk, it checks the shape
// and bounds of what it loads; whether the plane and the steps' prototypes
// exist is checked by Verify.
func (p *Pack) Stairwell(ctx context.Context, plane, id string) (Stairwell, bool, error) {
	row := p.conn.QueryRowContext(ctx, stairwellSQL("WHERE plane = ?2 AND id = ?3"), maxDocumentBytes, plane, id)
	stairwell, err := scanStairwell(row)
	if errors.Is(err, sql.ErrNoRows) {
		return Stairwell{}, false, nil
	}
	if err != nil {
		return Stairwell{}, false, sqliteError(ctx, err)
	}
	return stairwell, true, nil
}

// Stairwells yields the stairwells of a plane, in ID order. One stored
// document is held at a time.
func (p *Pack) Stairwells(ctx context.Context, plane string) iter.Seq2[Stairwell, error] {
	return query(ctx, p, stairwellSQL("WHERE plane = ?2 ORDER BY id"), scanStairwell, maxDocumentBytes, plane)
}

// StairwellStep returns the stairwell step on a tile of a plane, and
// whether there is one. It checks the shape and bounds of what it loads;
// whether the stairwell and prototype exist is checked by Verify.
func (p *Pack) StairwellStep(ctx context.Context, plane string, layer int32, x, y int64) (StairwellTile, bool, error) {
	var stairwell, proto sql.NullString
	var step sql.NullInt64
	err := p.conn.QueryRowContext(ctx,
		"SELECT "+boundedIdentifier("stairwell")+", "+stepIndex("step")+", "+boundedIdentifier("proto")+
			" FROM stairwell_steps WHERE plane = ?1 AND layer = ?2 AND x = ?3 AND y = ?4",
		plane, int64(layer), x, y,
	).Scan(&stairwell, &step, &proto)
	if errors.Is(err, sql.ErrNoRows) {
		return StairwellTile{}, false, nil
	}
	if err != nil {
		return StairwellTile{}, false, sqliteError(ctx, err)
	}
	if stairwell, err = nullableText(stairwell, CodeStairwellStep); err != nil {
		return StairwellTile{}, false, err
	}
	if proto, err = nullableText(proto, CodeStairwellStep); err != nil {
		return StairwellTile{}, false, err
	}
	if !stairwell.Valid || !isIdentifier(stairwell.String) || !step.Valid || !proto.Valid || !isIdentifier(proto.String) {
		return StairwellTile{}, false, reject(CodeStairwellStep,
			"a stairwell step's stairwell or proto is not an identifier, or its index is not one a source array can have")
	}
	return StairwellTile{Stairwell: stairwell.String, Step: int(step.Int64), Proto: proto.String}, true, nil
}

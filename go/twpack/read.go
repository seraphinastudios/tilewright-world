// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"iter"
	"unicode/utf8"
)

// The source format's chunk size, ground layer and layer height limits.
const (
	formatChunkSize   = 32
	maxGroundLayers   = 8
	maxLayerHeight    = 65_535
	maxRenderLayers   = 65_534
	maxProfileVersion = 64
)

func validUTF8(text string) bool {
	return utf8.ValidString(text)
}

// boundedText is SQL for column when it is text of at most parameter
// bytes, else NULL.
func boundedText(column, parameter string) string {
	return fmt.Sprintf("CASE WHEN typeof(%[1]s) = 'text' AND octet_length(%[1]s) <= %[2]s THEN %[1]s END", column, parameter)
}

// boundedIdentifier is SQL for column when it is text the length of an
// identifier, at most 128 bytes, else NULL; callers then check the pattern.
func boundedIdentifier(column string) string {
	return fmt.Sprintf("CASE WHEN typeof(%[1]s) = 'text' AND octet_length(%[1]s) BETWEEN 1 AND 128 THEN %[1]s END", column)
}

// boundedInteger is SQL for column when it is an integer from low to high,
// else NULL.
func boundedInteger(column string, low, high int64) string {
	return fmt.Sprintf("CASE WHEN typeof(%[1]s) = 'integer' AND %[1]s BETWEEN %[2]d AND %[3]d THEN %[1]s END", column, low, high)
}

// stepIndex is SQL for column when it is an index a source array can have,
// from 0 to one less than the array limit, else NULL.
func stepIndex(column string) string {
	return boundedInteger(column, 0, maxArrayItems-1)
}

// notIdentifier is SQL that is true when column is not an identifier. The
// length is checked first, without loading the value; GLOB stops at a NUL
// byte, so NUL is rejected separately.
func notIdentifier(column string) string {
	return fmt.Sprintf("(typeof(%[1]s) != 'text' OR octet_length(%[1]s) NOT BETWEEN 1 AND 128 "+
		"OR instr(%[1]s, char(0)) != 0 "+
		"OR %[1]s NOT GLOB '[a-z0-9]*' OR %[1]s GLOB '*[^a-z0-9_.-]*')", column)
}

// chunkDataSQL is SQL for a chunks c row's data: the blob when the row's
// encoding is this one and the blob is within MaxChunkBytes, else 0 for
// another encoding and 1 for other data. Nothing larger is loaded.
func chunkDataSQL() string {
	return fmt.Sprintf("CASE WHEN octet_length(c.encoding) = %d AND c.encoding = '%s' "+
		"THEN CASE WHEN typeof(c.data) = 'blob' AND octet_length(c.data) <= %d "+
		"THEN c.data ELSE 1 END ELSE 0 END", len(ChunkEncoding), ChunkEncoding, MaxChunkBytes)
}

// chunkBlob is the value of chunkDataSQL: the data, or the error for a row
// with another encoding or other data.
func chunkBlob(value any) ([]byte, error) {
	switch value := value.(type) {
	case []byte:
		return value, nil
	case nil:
		// An empty blob.
		return []byte{}, nil
	case int64:
		if value == 0 {
			return nil, reject(CodeChunkEncoding, "a chunk's encoding is not "+ChunkEncoding)
		}
	}
	return nil, reject(CodeChunkData, fmt.Sprintf("a chunk's data is not a blob of at most %d bytes", MaxChunkBytes))
}

// nullableText reads an optional text column that must be UTF-8: SQLite
// stores any bytes as text, so text that is not UTF-8 is code's error.
func nullableText(value sql.NullString, code Code) (sql.NullString, error) {
	if value.Valid && !validUTF8(value.String) {
		return value, reject(code, "a stored value is not UTF-8 text")
	}
	return value, nil
}

// storedDocument parses a stored document, which must be canonical JSON of
// an object whose custom data, if any, has namespace keys; code is the
// rule a bad one breaks.
func storedDocument(text sql.NullString, code Code, what string) (jsonValue, error) {
	invalid := reject(code, what+" is not canonical JSON of an object with namespaced custom data")
	if !text.Valid {
		return jsonValue{}, invalid
	}
	root, err := parseCanonical(text.String, maxDepth)
	if err != nil || root.kind != jsonObject {
		return jsonValue{}, invalid
	}
	// custom data, where a document has it, is keyed by namespaces as in
	// the source format.
	if custom, ok := root.get("custom"); ok {
		if custom.kind != jsonObject {
			return jsonValue{}, invalid
		}
		for _, member := range custom.children {
			if !isNamespace(member.key) {
				return jsonValue{}, invalid
			}
		}
	}
	return root, nil
}

// Summary holds the row counts of a pack.
type Summary struct {
	Strings     uint64
	Planes      uint64
	Zones       uint64
	Anchors     uint64
	Stairwells  uint64
	Chunks      uint64
	Definitions uint64
	// Objects counts the objects in every chunk, inventories included; only
	// Verify counts them.
	Objects uint64
}

// Summary returns the pack's row counts.
func (p *Pack) Summary(ctx context.Context) (Summary, error) {
	var summary Summary
	for _, count := range []struct {
		table string
		into  *uint64
	}{
		{"strings", &summary.Strings},
		{"planes", &summary.Planes},
		{"zones", &summary.Zones},
		{"anchors", &summary.Anchors},
		{"stairwells", &summary.Stairwells},
		{"chunks", &summary.Chunks},
		{"definitions", &summary.Definitions},
	} {
		var value int64
		if err := p.conn.QueryRowContext(ctx, "SELECT count(*) FROM "+count.table).Scan(&value); err != nil {
			return Summary{}, sqliteError(ctx, err)
		}
		if value > 0 {
			*count.into = uint64(value)
		}
	}
	return summary, nil
}

// Meta returns a meta value, and whether the pack has the key. Open has
// checked that meta holds exactly the format's keys.
func (p *Pack) Meta(ctx context.Context, key string) (string, bool, error) {
	var value sql.NullString
	err := p.conn.QueryRowContext(ctx, "SELECT value FROM meta WHERE key = ?1", key).Scan(&value)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	if err != nil {
		return "", false, sqliteError(ctx, err)
	}
	if value, err = nullableText(value, CodeMeta); err != nil {
		return "", false, err
	}
	return value.String, value.Valid, nil
}

// String returns an interned string, and whether the ID exists.
func (p *Pack) String(ctx context.Context, id uint32) (string, bool, error) {
	var value sql.NullString
	err := p.conn.QueryRowContext(ctx,
		"SELECT "+boundedText("value", "?1")+" FROM strings WHERE id = ?2",
		maxStringBytes, int64(id),
	).Scan(&value)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	if err != nil {
		return "", false, sqliteError(ctx, err)
	}
	if value, err = nullableText(value, CodeString); err != nil {
		return "", false, err
	}
	if !value.Valid {
		return "", false, reject(CodeString, "a string is not text of at most 1 MiB")
	}
	return value.String, true, nil
}

// Chunk decodes one chunk into its full model; see DecodeChunk. It returns
// false when the plane has no chunk there. Like every single read, it
// checks the shape and bounds of what it loads; whether the strings,
// definitions, zones and anchors the record names exist is checked by
// Verify.
func (p *Pack) Chunk(ctx context.Context, plane string, layer int32, cx, cy int64) (*Chunk, bool, error) {
	var value any
	err := p.conn.QueryRowContext(ctx,
		"SELECT "+chunkDataSQL()+" FROM chunks c WHERE c.plane = ?1 AND c.layer = ?2 AND c.cx = ?3 AND c.cy = ?4",
		plane, int64(layer), cx, cy,
	).Scan(&value)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, false, nil
	}
	if err != nil {
		return nil, false, sqliteError(ctx, err)
	}
	data, err := chunkBlob(value)
	if err != nil {
		return nil, false, err
	}
	chunk, err := DecodeChunk(data)
	if err != nil {
		return nil, false, err
	}
	return chunk, true, nil
}

// ChunkKey is the position of a chunk in its plane and layer.
type ChunkKey struct {
	CX, CY int64
}

// layersSQL lists a plane's layers. Grouping by the stored layer lets
// SQLite read them in primary-key order instead of collecting them first;
// the bounded value is what is loaded.
func layersSQL() string {
	return "SELECT " + boundedInteger("layer", -1<<31, 1<<31-1) +
		" FROM chunks WHERE plane = ?1 GROUP BY layer ORDER BY layer"
}

// chunkKeysSQL lists a layer's chunk positions, each NULL unless every tile
// of a format 1 chunk there is within the source format's integer range.
func chunkKeysSQL() string {
	low, high := int64(-(MaxSafeInteger / formatChunkSize)), int64((MaxSafeInteger-formatChunkSize+1)/formatChunkSize)
	return "SELECT " + boundedInteger("cx", low, high) + ", " + boundedInteger("cy", low, high) +
		" FROM chunks WHERE plane = ?1 AND layer = ?2 ORDER BY cx, cy"
}

// Layers yields the layers of a plane that have chunks, in order.
func (p *Pack) Layers(ctx context.Context, plane string) iter.Seq2[int32, error] {
	return func(yield func(int32, error) bool) {
		rows, err := p.conn.QueryContext(ctx, layersSQL(), plane)
		if err != nil {
			yield(0, sqliteError(ctx, err))
			return
		}
		defer rows.Close()
		for rows.Next() {
			var layer sql.NullInt64
			if err := rows.Scan(&layer); err != nil {
				yield(0, sqliteError(ctx, err))
				return
			}
			if !layer.Valid {
				yield(0, reject(CodeChunk, "a chunk's layer is not a 32-bit integer"))
				return
			}
			if !yield(int32(layer.Int64), nil) {
				return
			}
		}
		if err := rows.Err(); err != nil {
			yield(0, sqliteError(ctx, err))
		}
	}
}

// ChunkKeys yields the positions of a layer's chunks, by cx then cy. A
// position must keep every tile of its chunk within the source format's
// integer range.
func (p *Pack) ChunkKeys(ctx context.Context, plane string, layer int32) iter.Seq2[ChunkKey, error] {
	return func(yield func(ChunkKey, error) bool) {
		rows, err := p.conn.QueryContext(ctx, chunkKeysSQL(), plane, int64(layer))
		if err != nil {
			yield(ChunkKey{}, sqliteError(ctx, err))
			return
		}
		defer rows.Close()
		for rows.Next() {
			var cx, cy sql.NullInt64
			if err := rows.Scan(&cx, &cy); err != nil {
				yield(ChunkKey{}, sqliteError(ctx, err))
				return
			}
			if !cx.Valid || !cy.Valid {
				yield(ChunkKey{}, reject(CodeChunk, "a chunk's position puts tiles outside the source format's integer range"))
				return
			}
			if !yield(ChunkKey{CX: cx.Int64, CY: cy.Int64}, nil) {
				return
			}
		}
		if err := rows.Err(); err != nil {
			yield(ChunkKey{}, sqliteError(ctx, err))
		}
	}
}

// Plane is a plane row and its stored document.
type Plane struct {
	ID string
	// Kind is "shared", "instanced" or "per_player".
	Kind      string
	ChunkSize uint16
	// Document is the plane's source document as canonical JSON.
	Document string
}

func planeSQL(where string) string {
	return "SELECT " + boundedIdentifier("id") + ", " + boundedIdentifier("kind") + ", " +
		boundedInteger("chunk_size", formatChunkSize, formatChunkSize) + ", " +
		boundedText("document", "?1") + " FROM planes " + where
}

func scanPlane(rows interface{ Scan(...any) error }) (Plane, error) {
	var id, kind, document sql.NullString
	var chunkSize sql.NullInt64
	if err := rows.Scan(&id, &kind, &chunkSize, &document); err != nil {
		return Plane{}, err
	}
	invalid := reject(CodePlane, "a plane has an invalid ID or kind, or a chunk size other than 32")
	if !id.Valid || !isIdentifier(id.String) || !kind.Valid || !chunkSize.Valid {
		return Plane{}, invalid
	}
	switch kind.String {
	case "shared", "instanced", "per_player":
	default:
		return Plane{}, invalid
	}
	document, err := nullableText(document, CodePlane)
	if err != nil {
		return Plane{}, err
	}
	root, err := storedDocument(document, CodePlane, "a plane document")
	if err != nil {
		return Plane{}, err
	}
	if !planeAgrees(&root, id.String, kind.String, chunkSize.Int64) {
		return Plane{}, reject(CodePlane, "the document of plane "+id.String+" disagrees with its row")
	}
	return Plane{ID: id.String, Kind: kind.String, ChunkSize: uint16(chunkSize.Int64), Document: document.String}, nil
}

// planeAgrees reports whether a plane document has its row's identity and
// a default_zone, if any, that is a string; Verify checks that it names a
// zone of the plane.
func planeAgrees(root *jsonValue, id, kind string, chunkSize int64) bool {
	format, _ := root.str("format")
	documentID, _ := root.str("id")
	documentKind, _ := root.str("kind")
	size, ok := root.get("chunk_size")
	if format != "tilewright.plane/1" || documentID != id || documentKind != kind ||
		!ok || size.kind != jsonInteger || size.integer != chunkSize {
		return false
	}
	if zone, ok := root.get("default_zone"); ok && zone.kind != jsonString {
		return false
	}
	return true
}

// Plane returns a plane, and whether the pack has it.
func (p *Pack) Plane(ctx context.Context, id string) (Plane, bool, error) {
	row := p.conn.QueryRowContext(ctx, planeSQL("WHERE id = ?2"), maxDocumentBytes, id)
	plane, err := scanPlane(row)
	if errors.Is(err, sql.ErrNoRows) {
		return Plane{}, false, nil
	}
	if err != nil {
		return Plane{}, false, sqliteError(ctx, err)
	}
	return plane, true, nil
}

// Planes yields every plane, in ID order. One document is held at a time.
func (p *Pack) Planes(ctx context.Context) iter.Seq2[Plane, error] {
	return query(ctx, p, planeSQL("ORDER BY id"), scanPlane, maxDocumentBytes)
}

// query yields each row of a query, as scan reads it; it stops at the
// first error.
func query[T any](ctx context.Context, p *Pack, statement string, scan func(interface{ Scan(...any) error }) (T, error), arguments ...any) iter.Seq2[T, error] {
	return func(yield func(T, error) bool) {
		var zero T
		rows, err := p.conn.QueryContext(ctx, statement, arguments...)
		if err != nil {
			yield(zero, sqliteError(ctx, err))
			return
		}
		defer rows.Close()
		for rows.Next() {
			value, err := scan(rows)
			if err != nil {
				yield(zero, sqliteError(ctx, err))
				return
			}
			if !yield(value, nil) {
				return
			}
		}
		if err := rows.Err(); err != nil {
			yield(zero, sqliteError(ctx, err))
		}
	}
}

// Zone is a zone row and its stored entry.
type Zone struct {
	Plane string
	ID    string
	// Parent is the parent zone's ID, or "" for a root zone.
	Parent string
	// Document is the zone's source entry as canonical JSON.
	Document string
}

func zoneSQL(where string) string {
	return "SELECT " + boundedIdentifier("id") + ", parent IS NULL, " + boundedIdentifier("parent") + ", " +
		boundedText("document", "?1") + " FROM zones " + where
}

// scanZone reads the zones of plane.
func scanZone(plane string) func(interface{ Scan(...any) error }) (Zone, error) {
	return func(rows interface{ Scan(...any) error }) (Zone, error) {
		zone, err := scanZoneRow(rows)
		zone.Plane = plane
		return zone, err
	}
}

func scanZoneRow(rows interface{ Scan(...any) error }) (Zone, error) {
	var id, parent, document sql.NullString
	var root bool
	if err := rows.Scan(&id, &root, &parent, &document); err != nil {
		return Zone{}, err
	}
	if !id.Valid || !isIdentifier(id.String) || (!root && (!parent.Valid || !isIdentifier(parent.String))) {
		return Zone{}, reject(CodeZone, "a zone has an invalid ID or parent")
	}
	document, err := nullableText(document, CodeZone)
	if err != nil {
		return Zone{}, err
	}
	stored, err := storedDocument(document, CodeZone, "a zone document")
	if err != nil {
		return Zone{}, err
	}
	documentID, _ := stored.str("id")
	documentParent, hasParent := stored.get("parent")
	agrees := documentID == id.String && hasParent == !root &&
		(root || (documentParent.kind == jsonString && documentParent.text == parent.String))
	if !agrees {
		return Zone{}, reject(CodeZone, "the document of zone "+id.String+" disagrees with its row")
	}
	return Zone{ID: id.String, Parent: parent.String, Document: document.String}, nil
}

// Zone returns a zone of a plane, and whether the pack has it. Whether
// the plane and parent exist is checked by Verify.
func (p *Pack) Zone(ctx context.Context, plane, id string) (Zone, bool, error) {
	row := p.conn.QueryRowContext(ctx, zoneSQL("WHERE plane = ?2 AND id = ?3"), maxDocumentBytes, plane, id)
	zone, err := scanZone(plane)(row)
	if errors.Is(err, sql.ErrNoRows) {
		return Zone{}, false, nil
	}
	if err != nil {
		return Zone{}, false, sqliteError(ctx, err)
	}
	return zone, true, nil
}

// Zones yields the zones of a plane, in ID order.
func (p *Pack) Zones(ctx context.Context, plane string) iter.Seq2[Zone, error] {
	return query(ctx, p, zoneSQL("WHERE plane = ?2 ORDER BY id"), scanZone(plane), maxDocumentBytes, plane)
}

// Anchor is a named position on a plane.
type Anchor struct {
	Plane string
	ID    string
	Layer int32
	X, Y  int64
}

func anchorSQL(where string) string {
	return "SELECT " + boundedIdentifier("id") + ", " + boundedInteger("layer", -1<<31, 1<<31-1) + ", " +
		boundedInteger("x", -MaxSafeInteger, MaxSafeInteger) + ", " +
		boundedInteger("y", -MaxSafeInteger, MaxSafeInteger) + " FROM anchors " + where
}

// scanAnchor reads the anchors of plane.
func scanAnchor(plane string) func(interface{ Scan(...any) error }) (Anchor, error) {
	return func(rows interface{ Scan(...any) error }) (Anchor, error) {
		var id sql.NullString
		var layer, x, y sql.NullInt64
		if err := rows.Scan(&id, &layer, &x, &y); err != nil {
			return Anchor{}, err
		}
		if !id.Valid || !isIdentifier(id.String) || !layer.Valid || !x.Valid || !y.Valid {
			return Anchor{}, reject(CodeAnchor, "an anchor has an invalid ID, layer or position")
		}
		return Anchor{Plane: plane, ID: id.String, Layer: int32(layer.Int64), X: x.Int64, Y: y.Int64}, nil
	}
}

// Anchor returns an anchor of a plane, and whether the pack has it.
func (p *Pack) Anchor(ctx context.Context, plane, id string) (Anchor, bool, error) {
	row := p.conn.QueryRowContext(ctx, anchorSQL("WHERE plane = ?1 AND id = ?2"), plane, id)
	anchor, err := scanAnchor(plane)(row)
	if errors.Is(err, sql.ErrNoRows) {
		return Anchor{}, false, nil
	}
	if err != nil {
		return Anchor{}, false, sqliteError(ctx, err)
	}
	return anchor, true, nil
}

// Anchors yields the anchors of a plane, in ID order.
func (p *Pack) Anchors(ctx context.Context, plane string) iter.Seq2[Anchor, error] {
	return query(ctx, p, anchorSQL("WHERE plane = ?1 ORDER BY id"), scanAnchor(plane), plane)
}

// Definition is a definition row and its stored document.
type Definition struct {
	Type string
	ID   string
	// Document is the definition's source document as canonical JSON.
	Document string
}

func definitionSQL(where string) string {
	return "SELECT " + boundedIdentifier("type") + ", " + boundedIdentifier("id") + ", " +
		boundedText("document", "?1") + " FROM definitions " + where
}

func scanDefinition(rows interface{ Scan(...any) error }) (Definition, error) {
	var kind, id, document sql.NullString
	if err := rows.Scan(&kind, &id, &document); err != nil {
		return Definition{}, err
	}
	if !kind.Valid || !isIdentifier(kind.String) || !id.Valid || !isIdentifier(id.String) {
		return Definition{}, reject(CodeDefinition, "a definition type or ID is not an identifier")
	}
	document, err := nullableText(document, CodeDefinition)
	if err != nil {
		return Definition{}, err
	}
	stored, err := storedDocument(document, CodeDefinition, "a definition document")
	if err != nil {
		return Definition{}, err
	}
	format, _ := stored.str("format")
	documentType, _ := stored.str("type")
	documentID, _ := stored.str("id")
	if format != "tilewright.def/1" || documentType != kind.String || documentID != id.String {
		return Definition{}, reject(CodeDefinition, "the document of definition "+kind.String+"/"+id.String+" disagrees with its row")
	}
	return Definition{Type: kind.String, ID: id.String, Document: document.String}, nil
}

// Definition returns a definition, and whether the pack has it.
func (p *Pack) Definition(ctx context.Context, kind, id string) (Definition, bool, error) {
	row := p.conn.QueryRowContext(ctx, definitionSQL("WHERE type = ?2 AND id = ?3"), maxDocumentBytes, kind, id)
	definition, err := scanDefinition(row)
	if errors.Is(err, sql.ErrNoRows) {
		return Definition{}, false, nil
	}
	if err != nil {
		return Definition{}, false, sqliteError(ctx, err)
	}
	return definition, true, nil
}

// Definitions yields every definition, by type and then ID. One document
// is held at a time.
func (p *Pack) Definitions(ctx context.Context) iter.Seq2[Definition, error] {
	return query(ctx, p, definitionSQL("ORDER BY type, id"), scanDefinition, maxDocumentBytes)
}

// DefinitionsOfType yields the definitions of one type, in ID order.
func (p *Pack) DefinitionsOfType(ctx context.Context, kind string) iter.Seq2[Definition, error] {
	return query(ctx, p, definitionSQL("WHERE type = ?2 ORDER BY id"), scanDefinition, maxDocumentBytes, kind)
}

// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"strings"
)

// FileDigest returns the SHA-256 of the pack file, read from its path when
// called.
//
// This hashes whatever file is at the pack's path now, not necessarily the
// file SQLite opened: the digest binds to the path. Packs are
// content-addressed files opened from directories the caller controls; a
// directory that others can write to is not supported. It reads at most
// the pack size limit the pack was opened with, and fails for a longer
// file.
func (p *Pack) FileDigest(ctx context.Context) ([32]byte, error) {
	return p.digestWithin(ctx, p.maxBytes)
}

func (p *Pack) digestWithin(ctx context.Context, maximum uint64) ([32]byte, error) {
	file, err := os.Open(p.path)
	if err != nil {
		return [32]byte{}, ioError(err)
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return [32]byte{}, ioError(err)
	}
	if uint64(info.Size()) > maximum {
		return [32]byte{}, tooLarge(maximum)
	}
	// The length can change once checked, so the read stops one byte past
	// the limit.
	reader := io.LimitReader(file, int64(min(maximum, math.MaxInt64-1))+1)
	hasher := sha256.New()
	buffer := make([]byte, 64<<10)
	var total uint64
	for {
		if err := ctx.Err(); err != nil {
			return [32]byte{}, err
		}
		read, err := reader.Read(buffer)
		total += uint64(read)
		if total > maximum {
			return [32]byte{}, tooLarge(maximum)
		}
		hasher.Write(buffer[:read])
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return [32]byte{}, ioError(err)
		}
	}
	var digest [32]byte
	hasher.Sum(digest[:0])
	return digest, nil
}

// Verify checks the whole pack: SQLite integrity, the file name digest when
// the name is one, every string, plane, zone, anchor, stairwell, step and
// definition row, every stored document, every chunk record, and every
// string, layer, ground count and portal target a record references. It
// returns the pack's row counts, objects included.
//
// Decoding every chunk is linear in the pack's decoded size, which
// compression can make far larger than the file, so it stops and fails
// once limits.MaxDecodedBytes would be passed. The integrity check and the
// hash are linear in the file's size, so the pages are checked against the
// pack size limit before either runs, and the hash reads at most that many
// bytes. The limit is the lower of limits.MaxPackBytes and the one the
// pack was opened with, so verifying never reads more than opening
// allowed. The file name check hashes the file at the pack's path, so it
// binds to the path; see FileDigest.
func (p *Pack) Verify(ctx context.Context, limits Limits) (Summary, error) {
	limits = limits.withDefaults()
	maximum := min(limits.MaxPackBytes, p.maxBytes)
	pages, err := pragmaInteger(ctx, p.conn, "page_count")
	if err != nil {
		return Summary{}, err
	}
	if pages < 0 || uint64(pages) > maximum/PageSize {
		return Summary{}, tooLarge(maximum)
	}
	var integrity string
	if err := p.conn.QueryRowContext(ctx, "PRAGMA integrity_check").Scan(&integrity); err != nil {
		return Summary{}, sqliteError(ctx, err)
	}
	if integrity != "ok" {
		return Summary{}, reject(CodeIntegrity, "integrity check failed: "+integrity)
	}
	if stem, ok := strings.CutSuffix(filepath.Base(p.path), ".twpack"); ok && isHexDigest(stem) {
		// The name must be the lowercase digest; other cases do not match.
		digest, err := p.digestWithin(ctx, maximum)
		if err != nil {
			return Summary{}, err
		}
		if encoded := hex.EncodeToString(digest[:]); encoded != stem {
			return Summary{}, reject(CodeDigest, "file digest "+encoded+" does not match its name")
		}
	}
	return p.verifyContents(ctx, limits.MaxDecodedBytes)
}

// isHexDigest reports whether stem is 64 hex digits of either case.
func isHexDigest(stem string) bool {
	if len(stem) != 64 {
		return false
	}
	for i := 0; i < len(stem); i++ {
		c := stem[i]
		if !isDigit(c) && (c < 'a' || c > 'f') && (c < 'A' || c > 'F') {
			return false
		}
	}
	return true
}

// verifyContents checks everything Verify checks except SQLite integrity
// and the file name.
func (p *Pack) verifyContents(ctx context.Context, maxDecoded uint64) (Summary, error) {
	summary, err := p.Summary(ctx)
	if err != nil {
		return Summary{}, err
	}
	var minimum, maximum sql.NullInt64
	if err := p.conn.QueryRowContext(ctx, "SELECT min(id), max(id) FROM strings").Scan(&minimum, &maximum); err != nil {
		return Summary{}, sqliteError(ctx, err)
	}
	contiguous := (!minimum.Valid && !maximum.Valid && summary.Strings == 0) ||
		(minimum.Valid && minimum.Int64 == 1 && maximum.Valid && maximum.Int64 >= 0 && uint64(maximum.Int64) == summary.Strings)
	if !contiguous {
		return Summary{}, reject(CodeStringIDs, "string IDs are not 1..=count")
	}
	if err := p.checkRows(ctx); err != nil {
		return Summary{}, err
	}
	if err := p.checkStringEncoding(ctx); err != nil {
		return Summary{}, err
	}
	if err := p.checkDocuments(ctx); err != nil {
		return Summary{}, err
	}
	if err := p.checkStairwells(ctx); err != nil {
		return Summary{}, err
	}
	profile, err := p.Profile(ctx)
	if err != nil {
		return Summary{}, err
	}
	if summary.Strings > math.MaxUint32 {
		return Summary{}, reject(CodeStringIDs, "too many strings")
	}
	targets, err := newTargets(ctx, p.conn, uint32(summary.Strings), len(profile.RenderLayers))
	if err != nil {
		return Summary{}, err
	}
	defer targets.close()
	objects, checked, err := p.checkChunks(ctx, targets, len(profile.GroundLayers), maxDecoded)
	if err != nil {
		return Summary{}, err
	}
	if checked != summary.Chunks {
		return Summary{}, reject(CodeChunk, "a chunk names an unknown plane")
	}
	summary.Objects = objects
	return summary, nil
}

func (p *Pack) checkChunks(ctx context.Context, targets *targets, groundLayers int, maxDecoded uint64) (objects, checked uint64, err error) {
	rows, err := p.conn.QueryContext(ctx,
		"SELECT p.id, p.chunk_size, "+chunkDataSQL()+
			" FROM chunks c JOIN planes p ON p.id = c.plane ORDER BY c.plane, c.layer, c.cx, c.cy")
	if err != nil {
		return 0, 0, sqliteError(ctx, err)
	}
	defer rows.Close()
	var decoded uint64
	for rows.Next() {
		var plane string
		var chunkSize int64
		var value any
		if err := rows.Scan(&plane, &chunkSize, &value); err != nil {
			return 0, 0, sqliteError(ctx, err)
		}
		// The frame records its decoded size, so the budget is checked
		// before the chunk is decompressed.
		if data, ok := value.([]byte); ok {
			size := recordedSize(data)
			if decoded+size < decoded {
				decoded = math.MaxUint64
			} else {
				decoded += size
			}
		}
		if decoded > maxDecoded {
			return 0, 0, reject(CodeDecodeBudget, fmt.Sprintf("the chunks decode to more than %d bytes, the verification limit", maxDecoded))
		}
		data, err := chunkBlob(value)
		if err != nil {
			return 0, 0, err
		}
		// Objects are checked as they decode rather than kept, so memory
		// stays bounded by the record, not by the objects it holds.
		targets.enter(plane)
		chunk, err := decodeChunk(data, func(object *Object) error {
			objects++
			return targets.check(object)
		})
		if err != nil {
			return 0, 0, err
		}
		if int64(chunk.ChunkSize) != chunkSize || len(chunk.Ground) != groundLayers {
			return 0, 0, reject(CodeChunk, "a chunk disagrees with its plane or profile")
		}
		if err := targets.checkPalette(chunk); err != nil {
			return 0, 0, err
		}
		checked++
	}
	if err := rows.Err(); err != nil {
		return 0, 0, sqliteError(ctx, err)
	}
	return objects, checked, nil
}

// checkRows checks string, plane, zone, anchor, stairwell, step, chunk and
// definition rows on their own and against each other, without loading
// them.
func (p *Pack) checkRows(ctx context.Context) error {
	safe := fmt.Sprint(MaxSafeInteger)
	checks := []struct {
		query   string
		code    Code
		problem string
	}{
		{
			fmt.Sprintf("SELECT count(*) FROM strings WHERE typeof(value) != 'text' OR octet_length(value) > %d OR instr(value, char(0)) != 0", maxStringBytes),
			CodeString, "a string is not text of at most 1 MiB without NUL",
		},
		{
			"SELECT count(*) FROM strings a JOIN strings b ON b.id = a.id + 1 WHERE b.value <= a.value",
			CodeStringIDs, "string IDs are not in byte order of their values",
		},
		{
			fmt.Sprintf("SELECT count(*) FROM planes WHERE %s OR kind NOT IN ('shared', 'instanced', 'per_player') OR typeof(chunk_size) != 'integer' OR chunk_size != %d",
				notIdentifier("id"), formatChunkSize),
			CodePlane, "a plane has an invalid ID or kind, or a chunk size other than 32",
		},
		{
			fmt.Sprintf("SELECT count(*) FROM zones z WHERE %s OR NOT EXISTS (SELECT 1 FROM planes p WHERE p.id = z.plane) OR (z.parent IS NOT NULL AND NOT EXISTS (SELECT 1 FROM zones q WHERE q.plane = z.plane AND q.id = z.parent))",
				notIdentifier("z.id")),
			CodeZone, "a zone has an invalid ID or names an unknown plane or parent",
		},
		{
			// Zones reached from a root by following children: with one
			// parent each, a zone is reached once or, on or below a cycle,
			// never.
			"WITH RECURSIVE children(plane, parent, id) AS MATERIALIZED " +
				"(SELECT plane, parent, id FROM zones WHERE parent IS NOT NULL), " +
				"rooted(plane, id) AS (SELECT plane, id FROM zones WHERE parent IS NULL " +
				"UNION ALL SELECT c.plane, c.id FROM rooted r JOIN children c ON c.plane = r.plane AND c.parent = r.id) " +
				"SELECT (SELECT count(*) FROM zones) - (SELECT count(*) FROM rooted)",
			CodeZone, "zone parents form a cycle",
		},
		{
			fmt.Sprintf("SELECT count(*) FROM anchors a WHERE %s OR NOT EXISTS (SELECT 1 FROM planes p WHERE p.id = a.plane) OR typeof(a.layer) != 'integer' OR a.layer NOT BETWEEN -2147483648 AND 2147483647 OR typeof(a.x) != 'integer' OR a.x NOT BETWEEN -%[2]s AND %[2]s OR typeof(a.y) != 'integer' OR a.y NOT BETWEEN -%[2]s AND %[2]s",
				notIdentifier("a.id"), safe),
			CodeAnchor, "an anchor has an invalid ID, layer or position, or names an unknown plane",
		},
		{
			fmt.Sprintf("SELECT count(*) FROM stairwells s WHERE %s OR NOT EXISTS (SELECT 1 FROM planes p WHERE p.id = s.plane)",
				notIdentifier("s.id")),
			CodeStairwell, "a stairwell has an invalid ID or names an unknown plane",
		},
		{
			// A step's stairwell is a stairwell of its plane, so the plane
			// exists too.
			fmt.Sprintf("SELECT count(*) FROM stairwell_steps t WHERE NOT EXISTS (SELECT 1 FROM stairwells s WHERE s.plane = t.plane AND s.id = t.stairwell) "+
				"OR typeof(t.layer) != 'integer' OR t.layer NOT BETWEEN -2147483648 AND 2147483647 "+
				"OR typeof(t.x) != 'integer' OR t.x NOT BETWEEN -%[1]s AND %[1]s "+
				"OR typeof(t.y) != 'integer' OR t.y NOT BETWEEN -%[1]s AND %[1]s "+
				"OR %[2]s IS NULL "+
				"OR %[3]s OR NOT EXISTS (SELECT 1 FROM definitions d WHERE d.type = 'proto' AND d.id = t.proto)",
				safe, stepIndex("t.step"), notIdentifier("t.proto")),
			CodeStairwellStep, "a stairwell step names an unknown stairwell or proto, or has an invalid layer, position or index",
		},
		{
			// Chunk c covers tiles c * size ..= c * size + size - 1.
			fmt.Sprintf("SELECT count(*) FROM chunks c LEFT JOIN planes p ON p.id = c.plane WHERE p.id IS NULL "+
				"OR typeof(c.layer) != 'integer' OR c.layer NOT BETWEEN -2147483648 AND 2147483647 "+
				"OR typeof(c.cx) != 'integer' OR typeof(c.cy) != 'integer' "+
				"OR c.cx NOT BETWEEN -(%[1]s / p.chunk_size) AND (%[1]s - p.chunk_size + 1) / p.chunk_size "+
				"OR c.cy NOT BETWEEN -(%[1]s / p.chunk_size) AND (%[1]s - p.chunk_size + 1) / p.chunk_size", safe),
			CodeChunk, "a chunk names an unknown plane or has an invalid layer or position",
		},
		{
			fmt.Sprintf("SELECT count(*) FROM definitions WHERE %s OR %s", notIdentifier("type"), notIdentifier("id")),
			CodeDefinition, "a definition type or ID is not an identifier",
		},
	}
	for _, check := range checks {
		var count int64
		if err := p.conn.QueryRowContext(ctx, check.query).Scan(&count); err != nil {
			return sqliteError(ctx, err)
		}
		if count != 0 {
			return reject(check.code, check.problem)
		}
	}
	return nil
}

// checkStringEncoding checks that every string is UTF-8, one bounded row at
// a time; SQLite accepts any bytes as text.
func (p *Pack) checkStringEncoding(ctx context.Context) error {
	rows, err := p.conn.QueryContext(ctx, "SELECT value FROM strings ORDER BY id")
	if err != nil {
		return sqliteError(ctx, err)
	}
	defer rows.Close()
	for rows.Next() {
		var value sql.RawBytes
		if err := rows.Scan(&value); err != nil {
			return sqliteError(ctx, err)
		}
		if !validUTF8(string(value)) {
			return reject(CodeString, "a string is not UTF-8 text")
		}
	}
	if err := rows.Err(); err != nil {
		return sqliteError(ctx, err)
	}
	return nil
}

// checkDocuments checks that every document column holds canonical JSON
// of an object whose identity keys agree with its row, and that a plane's
// default_zone is one of its zones.
func (p *Pack) checkDocuments(ctx context.Context) error {
	document := boundedText("document", "?1")
	hasZone, err := p.conn.PrepareContext(ctx, "SELECT EXISTS (SELECT 1 FROM zones WHERE plane = ?1 AND id = ?2)")
	if err != nil {
		return sqliteError(ctx, err)
	}
	defer hasZone.Close()
	err = eachRow(ctx, p.conn, "SELECT id, kind, chunk_size, "+document+" FROM planes ORDER BY id", []any{maxDocumentBytes},
		func(scan func(...any) error) error {
			var id, kind string
			var chunkSize int64
			var text sql.NullString
			if err := scan(&id, &kind, &chunkSize, &text); err != nil {
				return err
			}
			text, err := nullableText(text, CodePlane)
			if err != nil {
				return err
			}
			stored, err := storedDocument(text, CodePlane, "a plane document")
			if err != nil {
				return err
			}
			agrees := planeAgrees(&stored, id, kind, chunkSize)
			if zone, ok := stored.get("default_zone"); agrees && ok {
				if err := hasZone.QueryRowContext(ctx, id, zone.text).Scan(&agrees); err != nil {
					return err
				}
			}
			if !agrees {
				return reject(CodePlane, "the document of plane "+id+" disagrees with its row")
			}
			return nil
		})
	if err != nil {
		return err
	}
	err = eachRow(ctx, p.conn, "SELECT plane, id, parent, "+document+" FROM zones ORDER BY plane, id", []any{maxDocumentBytes},
		func(scan func(...any) error) error {
			var plane, id string
			var parent, text sql.NullString
			if err := scan(&plane, &id, &parent, &text); err != nil {
				return err
			}
			text, err := nullableText(text, CodeZone)
			if err != nil {
				return err
			}
			stored, err := storedDocument(text, CodeZone, "a zone document")
			if err != nil {
				return err
			}
			documentID, _ := stored.str("id")
			documentParent, hasParent := stored.get("parent")
			agrees := documentID == id && hasParent == parent.Valid &&
				(!hasParent || (documentParent.kind == jsonString && documentParent.text == parent.String))
			if !agrees {
				return reject(CodeZone, "the document of zone "+plane+"/"+id+" disagrees with its row")
			}
			return nil
		})
	if err != nil {
		return err
	}
	return eachRow(ctx, p.conn, "SELECT type, id, "+document+" FROM definitions ORDER BY type, id", []any{maxDocumentBytes},
		func(scan func(...any) error) error {
			var kind, id string
			var text sql.NullString
			if err := scan(&kind, &id, &text); err != nil {
				return err
			}
			text, err := nullableText(text, CodeDefinition)
			if err != nil {
				return err
			}
			stored, err := storedDocument(text, CodeDefinition, "a definition document")
			if err != nil {
				return err
			}
			format, _ := stored.str("format")
			documentType, _ := stored.str("type")
			documentID, _ := stored.str("id")
			if format != "tilewright.def/1" || documentType != kind || documentID != id {
				return reject(CodeDefinition, "the document of definition "+kind+"/"+id+" disagrees with its row")
			}
			return nil
		})
}

// eachRow calls visit with each row of a query, stopping at its first
// error.
func eachRow(ctx context.Context, conn *sql.Conn, statement string, arguments []any, visit func(scan func(...any) error) error) error {
	rows, err := conn.QueryContext(ctx, statement, arguments...)
	if err != nil {
		return sqliteError(ctx, err)
	}
	defer rows.Close()
	for rows.Next() {
		if err := visit(rows.Scan); err != nil {
			return sqliteError(ctx, err)
		}
	}
	if err := rows.Err(); err != nil {
		return sqliteError(ctx, err)
	}
	return nil
}

// checkStairwells checks every stored stairwell document against its row
// and the stairwell_steps table: each step is the row at its tile, naming
// its stairwell, its index and its prototype, and the table holds no other
// rows. One document is held at a time, and each step is one lookup.
// checkRows has already checked the rows' columns.
func (p *Pack) checkStairwells(ctx context.Context) error {
	stepAt, err := p.conn.PrepareContext(ctx,
		"SELECT stairwell, step, proto FROM stairwell_steps WHERE plane = ?1 AND layer = ?2 AND x = ?3 AND y = ?4")
	if err != nil {
		return sqliteError(ctx, err)
	}
	defer stepAt.Close()
	var listed uint64
	// Stairwells and steps of the plane being checked: one source document
	// holds at most the array limit of stairwells and the value limit of
	// steps.
	current, count, steps := "", 0, 0
	err = eachRow(ctx, p.conn, "SELECT plane, id, "+boundedText("document", "?1")+" FROM stairwells ORDER BY plane, id", []any{maxDocumentBytes},
		func(scan func(...any) error) error {
			var plane, id string
			var text sql.NullString
			if err := scan(&plane, &id, &text); err != nil {
				return err
			}
			if plane != current {
				current, count, steps = plane, 0, 0
			}
			count++
			if count > maxArrayItems {
				return reject(CodeStairwell, "plane "+plane+" has more stairwells than a source document holds")
			}
			text, err := nullableText(text, CodeStairwell)
			if err != nil {
				return err
			}
			root, err := storedDocument(text, CodeStairwell, "a stairwell document")
			if err != nil {
				return err
			}
			stored, err := storedStairwell(&root, text.String)
			if err != nil {
				return err
			}
			steps += len(stored.Steps)
			if steps > maxValues {
				return reject(CodeStairwell, "plane "+plane+" has more stairwell steps than a source document holds")
			}
			disagrees := reject(CodeStairwell, "the document of stairwell "+plane+"/"+id+" disagrees with its rows")
			if stored.ID != id {
				return disagrees
			}
			for index, step := range stored.Steps {
				var stairwell, proto string
				var stepIndex int64
				err := stepAt.QueryRowContext(ctx, plane, int64(step.Layer), step.X, step.Y).Scan(&stairwell, &stepIndex, &proto)
				if errors.Is(err, sql.ErrNoRows) {
					return disagrees
				}
				if err != nil {
					return err
				}
				if stairwell != id || stepIndex != int64(index) || proto != step.Proto {
					return disagrees
				}
			}
			listed += uint64(len(stored.Steps))
			return nil
		})
	if err != nil {
		return err
	}
	var rows int64
	if err := p.conn.QueryRowContext(ctx, "SELECT count(*) FROM stairwell_steps").Scan(&rows); err != nil {
		return sqliteError(ctx, err)
	}
	if rows < 0 || uint64(rows) != listed {
		return reject(CodeStairwellStep, "stairwell_steps holds a step that no stairwell lists")
	}
	return nil
}

// cacheEntries is the number of entries kept per lookup cache. A full cache
// is cleared, so memory stays bounded however many distinct targets a pack
// names, and a lookup is at worst one query per object.
const cacheEntries = 1 << 16

// cached returns a cached boolean lookup, looking it up when it is not
// cached.
func cached[K comparable](cache map[K]bool, key K, lookup func() (bool, error)) (bool, error) {
	if known, ok := cache[key]; ok {
		return known, nil
	}
	found, err := lookup()
	if err != nil {
		return false, err
	}
	if len(cache) >= cacheEntries {
		clear(cache)
	}
	cache[key] = found
	return found, nil
}

// targets is what chunk records may reference. Strings are checked by ID,
// since verification has proven IDs are 1..strings; every other lookup is
// cached per distinct ID in a cache of bounded size.
type targets struct {
	ctx          context.Context
	strings      uint32
	renderLayers int
	// isPlane: whether string ?1 names a plane. isProto: whether string ?1
	// names a proto definition. isZone: whether string ?1 names a zone of
	// plane ?2. hasAnchor: whether plane ?1 (a string ID, or 0 for plane
	// ?2) has the anchor named by string ?3. value: the value of string ?1.
	isPlane, isProto, isZone, hasAnchor, value *sql.Stmt
	planes, protos, fieldNames                 map[uint32]bool
	// anchors caches anchors by (plane, anchor) for portals that name
	// their plane.
	anchors map[[2]uint32]bool
	// current is the plane of the chunks being checked, and zones and
	// ownAnchors the lookups relative to it.
	current           string
	zones, ownAnchors map[uint32]bool
}

func newTargets(ctx context.Context, conn *sql.Conn, strings uint32, renderLayers int) (*targets, error) {
	t := &targets{
		ctx: ctx, strings: strings, renderLayers: renderLayers,
		planes: map[uint32]bool{}, protos: map[uint32]bool{}, fieldNames: map[uint32]bool{},
		anchors: map[[2]uint32]bool{}, zones: map[uint32]bool{}, ownAnchors: map[uint32]bool{},
	}
	for _, statement := range []struct {
		into  **sql.Stmt
		query string
	}{
		{&t.isPlane, "SELECT EXISTS (SELECT 1 FROM strings s JOIN planes p ON p.id = s.value WHERE s.id = ?1)"},
		{&t.isProto, "SELECT EXISTS (SELECT 1 FROM strings s JOIN definitions d ON d.type = 'proto' AND d.id = s.value WHERE s.id = ?1)"},
		{&t.isZone, "SELECT EXISTS (SELECT 1 FROM strings s JOIN zones z ON z.plane = ?2 AND z.id = s.value WHERE s.id = ?1)"},
		{&t.hasAnchor, "SELECT EXISTS (SELECT 1 FROM strings n JOIN anchors a ON a.id = n.value " +
			"WHERE n.id = ?3 AND a.plane = CASE WHEN ?1 = 0 THEN ?2 ELSE (SELECT value FROM strings WHERE id = ?1) END)"},
		{&t.value, "SELECT value FROM strings WHERE id = ?1"},
	} {
		prepared, err := conn.PrepareContext(ctx, statement.query)
		if err != nil {
			t.close()
			return nil, sqliteError(ctx, err)
		}
		*statement.into = prepared
	}
	return t, nil
}

func (t *targets) close() {
	for _, statement := range []*sql.Stmt{t.isPlane, t.isProto, t.isZone, t.hasAnchor, t.value} {
		if statement != nil {
			_ = statement.Close()
		}
	}
}

func (t *targets) exists(statement *sql.Stmt, arguments ...any) (bool, error) {
	var found bool
	if err := statement.QueryRowContext(t.ctx, arguments...).Scan(&found); err != nil {
		return false, sqliteError(t.ctx, err)
	}
	return found, nil
}

func (t *targets) string(id uint32) bool {
	return id >= 1 && id <= t.strings
}

// enter starts checking chunks of plane, dropping lookups relative to the
// previous one.
func (t *targets) enter(plane string) {
	if t.current != plane {
		t.current = plane
		clear(t.zones)
		clear(t.ownAnchors)
	}
}

func (t *targets) proto(id uint32) (bool, error) {
	if !t.string(id) {
		return false, nil
	}
	return cached(t.protos, id, func() (bool, error) { return t.exists(t.isProto, int64(id)) })
}

func (t *targets) fieldName(id uint32) (bool, error) {
	if !t.string(id) {
		return false, nil
	}
	return cached(t.fieldNames, id, func() (bool, error) {
		var name string
		if err := t.value.QueryRowContext(t.ctx, int64(id)).Scan(&name); err != nil {
			return false, sqliteError(t.ctx, err)
		}
		return isFieldName(name), nil
	})
}

// checkPalette checks that every palette entry is used, that ground cells
// name proto definitions, and that zone cells name zones of the current
// plane.
func (t *targets) checkPalette(chunk *Chunk) error {
	mark := func(cells []uint16, used []bool) {
		for _, cell := range cells {
			if cell >= 1 && int(cell) <= len(used) {
				used[cell-1] = true
			}
		}
	}
	ground := make([]bool, len(chunk.Palette))
	zone := make([]bool, len(chunk.Palette))
	for _, grid := range chunk.Ground {
		mark(grid, ground)
	}
	mark(chunk.Zones, zone)
	for i, id := range chunk.Palette {
		ok := (ground[i] || zone[i]) && t.string(id)
		if ok && ground[i] {
			found, err := t.proto(id)
			if err != nil {
				return err
			}
			ok = found
		}
		if ok && zone[i] {
			found, err := cached(t.zones, id, func() (bool, error) { return t.exists(t.isZone, int64(id), t.current) })
			if err != nil {
				return err
			}
			ok = found
		}
		if !ok {
			return reject(CodeReference, "a palette entry is unused or not a proto definition or zone of its plane")
		}
	}
	return nil
}

func inSafeRange(value int64) bool {
	return value >= -MaxSafeInteger && value <= MaxSafeInteger
}

func (t *targets) portal(portal *Portal) (bool, error) {
	switch {
	case portal == nil:
		return true, nil
	case portal.Kind == PortalLocation:
		if !inSafeRange(portal.X) || !inSafeRange(portal.Y) {
			return false, nil
		}
		if portal.Plane == 0 {
			return true, nil
		}
		if !t.string(portal.Plane) {
			return false, nil
		}
		return cached(t.planes, portal.Plane, func() (bool, error) { return t.exists(t.isPlane, int64(portal.Plane)) })
	default:
		if !(portal.Plane == 0 || t.string(portal.Plane)) || !t.string(portal.Anchor) {
			return false, nil
		}
		lookup := func() (bool, error) {
			return t.exists(t.hasAnchor, int64(portal.Plane), t.current, int64(portal.Anchor))
		}
		if portal.Plane == 0 {
			return cached(t.ownAnchors, portal.Anchor, lookup)
		}
		return cached(t.anchors, [2]uint32{portal.Plane, portal.Anchor}, lookup)
	}
}

func (t *targets) check(object *Object) error {
	ok, err := t.proto(object.Proto)
	if err != nil {
		return err
	}
	ok = ok && int(object.Layer) <= t.renderLayers
	if ok {
		if ok, err = t.portal(object.Portal); err != nil {
			return err
		}
	}
	// Field names are field-name strings, each at most once per object.
	names := make(map[uint32]struct{}, len(object.Fields))
	for i := range object.Fields {
		field := &object.Fields[i]
		_, repeated := names[field.Name]
		names[field.Name] = struct{}{}
		if repeated {
			ok = false
		} else {
			named, err := t.fieldName(field.Name)
			if err != nil {
				return err
			}
			ok = ok && named
		}
		switch field.Kind {
		case FieldString:
			ok = ok && t.string(field.String)
		case FieldInteger:
			ok = ok && inSafeRange(field.Integer)
		}
	}
	if !ok {
		return reject(CodeReference, "an object references a missing string, layer, plane or anchor, repeats or misnames a field, or has a portal position or integer field out of range")
	}
	return nil
}

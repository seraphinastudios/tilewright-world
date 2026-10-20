// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"encoding/binary"
	"fmt"
	"math"
	"sync"
	"unicode/utf8"

	"github.com/klauspost/compress/zstd"
)

// The twc1+zstd chunk encoding (docs/spec/PACK.md section 2).
const (
	// ChunkEncoding is the chunks.encoding value of this encoding.
	ChunkEncoding = "twc1+zstd"
	// MaxRecordBytes is the largest decompressed record accepted.
	MaxRecordBytes = 16 << 20
	// MaxChunkBytes is the largest stored (compressed) chunk accepted:
	// zstd's compression bound for a record of MaxRecordBytes.
	MaxChunkBytes = MaxRecordBytes + MaxRecordBytes>>8
	// MaxInventoryDepth is the deepest inventory nesting accepted.
	MaxInventoryDepth = 16
	// NoTile is the tile of an inventory entry.
	NoTile = 0xFFFF
)

// RecordVersion is the version of a twc1 record of this format. Version 1
// records had no uids; readers accept only this version.
const RecordVersion = 2

const (
	recordVersion  = RecordVersion
	flagPortal     = 1
	flagFields     = 2
	flagCustom     = 4
	flagInventory  = 8
	flagUID        = 16
	maxGroundCount = 8
	maxChunkSize   = 255
	// minimumObjectBytes is the fixed part of an object record.
	minimumObjectBytes = 15
)

// Chunk is a decoded chunk record. Strings are IDs into the pack's strings
// table; see Pack.String.
type Chunk struct {
	ChunkSize uint16
	// Palette holds the string IDs the grids use, nonzero and strictly
	// ascending.
	Palette []uint32
	// Ground holds one grid per profile ground layer, in profile order:
	// ChunkSize² cells, row-major, each a palette index plus one, or 0 for
	// empty.
	Ground [][]uint16
	// Heights holds each tile's height, relative to its layer.
	Heights []int32
	// Zones holds each tile's zone: a palette index plus one, or 0 for the
	// plane's default zone.
	Zones   []uint16
	Objects []Object
}

// Object is an object record.
type Object struct {
	// UID is the object's uid, an identifier unique in the pack, or "" when
	// it has none.
	UID string
	// Tile is y * ChunkSize + x, or NoTile for an inventory entry.
	Tile  uint16
	Proto uint32
	// Layer is a render layer index plus one, or 0 for the prototype's
	// layer.
	Layer    uint16
	SubLayer int16
	Z        int32
	Portal   *Portal
	Fields   []Field
	// Custom is canonical JSON of the object's custom data, or "" when it
	// has none.
	Custom    string
	Inventory []Object
}

// PortalKind is how a portal names its target.
type PortalKind uint8

// Portal kinds.
const (
	PortalLocation PortalKind = 0
	PortalAnchor   PortalKind = 1
)

// Portal is an object's portal. Plane is a string ID, or 0 for the same
// plane. A location portal has Layer, X and Y; an anchor portal has Anchor,
// a string ID.
type Portal struct {
	Kind   PortalKind
	Plane  uint32
	Layer  int32
	X, Y   int64
	Anchor uint32
}

// FieldKind is the type of a field value.
type FieldKind uint8

// Field value kinds.
const (
	FieldNull FieldKind = iota
	FieldBool
	FieldInteger
	FieldNumber
	FieldString
	FieldJSON
)

// Field is one field of an object: Name is a string ID, and the value is
// the member that Kind names. String is a string ID, and JSON is canonical
// JSON of an array or object.
type Field struct {
	Name    uint32
	Kind    FieldKind
	Bool    bool
	Integer int64
	Number  float64
	String  uint32
	JSON    string
}

func recordError(format string, arguments ...any) error {
	return &Error{Code: CodeChunkRecord, Message: "invalid chunk record: " + fmt.Sprintf(format, arguments...)}
}

// DecodeChunk decompresses and decodes stored chunk data. A decoded model
// takes more memory than its record: up to several times the record, which
// is at most MaxRecordBytes.
func DecodeChunk(data []byte) (*Chunk, error) {
	return decodeChunk(data, nil)
}

// DecodeRecord decodes an uncompressed twc1 record.
func DecodeRecord(record []byte) (*Chunk, error) {
	return decodeRecord(record, nil)
}

// visitor receives each object of a record as it is decoded, at any depth
// and without its inventory; its inventory entries follow it.
type visitor func(*Object) error

var decoder = sync.OnceValues(func() (*zstd.Decoder, error) {
	return zstd.NewReader(nil,
		// Output is bounded by the frame's recorded content size, which is
		// checked first; the window then only bounds back-references within
		// that output, as for zstd's one-shot decompression.
		zstd.WithDecoderMaxMemory(1<<32),
		zstd.WithDecoderMaxWindow(maxWindowSize),
		zstd.WithDecodeAllCapLimit(true),
	)
})

const (
	zstdMagic       = 0xFD2FB528
	skippableMagic  = 0x184D2A50
	skippableMask   = 0xFFFFFFF0
	maxWindowLog    = 31
	maxWindowSize   = 1<<maxWindowLog + 7<<(maxWindowLog-3)
	blockHeaderSize = 3
)

// frameHeader is the part of a zstd frame header this reader needs.
type frameHeader struct {
	size        int
	contentSize uint64
	hasSize     bool
	checksum    bool
	skippable   bool
}

// parseFrameHeader reads a frame header as zstd does, rejecting reserved
// bits and windows past zstd's 64-bit limit.
func parseFrameHeader(data []byte) (frameHeader, bool) {
	if len(data) < 4 {
		return frameHeader{}, false
	}
	magic := binary.LittleEndian.Uint32(data)
	if magic&skippableMask == skippableMagic {
		if len(data) < 8 {
			return frameHeader{}, false
		}
		length := uint64(binary.LittleEndian.Uint32(data[4:]))
		return frameHeader{size: 8, contentSize: length, skippable: true}, true
	}
	if magic != zstdMagic || len(data) < 5 {
		return frameHeader{}, false
	}
	descriptor := data[4]
	if descriptor&0x08 != 0 {
		return frameHeader{}, false
	}
	singleSegment := descriptor&0x20 != 0
	sizeFlag := descriptor >> 6
	position := 5
	if !singleSegment {
		if len(data) <= position || 10+int(data[position]>>3) > maxWindowLog {
			return frameHeader{}, false
		}
		position++
	}
	position += [4]int{0, 1, 2, 4}[descriptor&3]
	sizeBytes := [4]int{0, 2, 4, 8}[sizeFlag]
	if sizeFlag == 0 && singleSegment {
		sizeBytes = 1
	}
	if len(data) < position+sizeBytes {
		return frameHeader{}, false
	}
	header := frameHeader{checksum: descriptor&0x04 != 0, hasSize: sizeBytes > 0}
	switch sizeBytes {
	case 1:
		header.contentSize = uint64(data[position])
	case 2:
		header.contentSize = uint64(binary.LittleEndian.Uint16(data[position:])) + 256
	case 4:
		header.contentSize = uint64(binary.LittleEndian.Uint32(data[position:]))
	case 8:
		header.contentSize = binary.LittleEndian.Uint64(data[position:])
	}
	header.size = position + sizeBytes
	return header, true
}

// frameLength is the length of the zstd frame data starts with, found by
// walking its block headers as zstd does, or false when it is malformed.
func frameLength(data []byte, header frameHeader) (int, bool) {
	if header.skippable {
		length := uint64(header.size) + header.contentSize
		if length > uint64(len(data)) {
			return 0, false
		}
		return int(length), true
	}
	position := header.size
	for {
		if len(data)-position < blockHeaderSize {
			return 0, false
		}
		block := uint32(data[position]) | uint32(data[position+1])<<8 | uint32(data[position+2])<<16
		position += blockHeaderSize
		last := block&1 != 0
		size := int(block >> 3)
		switch (block >> 1) & 3 {
		case 1:
			size = 1
		case 3:
			return 0, false
		}
		if len(data)-position < size {
			return 0, false
		}
		position += size
		if last {
			break
		}
	}
	if header.checksum {
		if len(data)-position < 4 {
			return 0, false
		}
		position += 4
	}
	return position, true
}

// recordedSize is the decoded size data's zstd frame records, or 0 when it
// records none.
func recordedSize(data []byte) uint64 {
	header, ok := parseFrameHeader(data)
	if !ok || header.skippable || !header.hasSize {
		return 0
	}
	return header.contentSize
}

func decodeChunk(data []byte, visit visitor) (*Chunk, error) {
	if len(data) > MaxChunkBytes {
		return nil, recordError("stored chunk exceeds %d bytes", MaxChunkBytes)
	}
	// Exactly one frame, so the size it records is the whole decoded size.
	header, ok := parseFrameHeader(data)
	if !ok {
		return nil, recordError("not exactly one zstd frame")
	}
	if length, ok := frameLength(data, header); !ok || length != len(data) {
		return nil, recordError("not exactly one zstd frame")
	}
	if header.skippable || !header.hasSize {
		return nil, recordError("not a zstd frame with a recorded content size")
	}
	if header.contentSize > MaxRecordBytes {
		return nil, recordError("record exceeds 16 MiB")
	}
	zstdDecoder, err := decoder()
	if err != nil {
		return nil, &Error{Code: CodeChunkRecord, Message: "zstd is unavailable", Err: err}
	}
	record, err := zstdDecoder.DecodeAll(data, make([]byte, 0, header.contentSize))
	if err != nil {
		return nil, recordError("zstd decompression failed: %v", err)
	}
	if uint64(len(record)) != header.contentSize {
		return nil, recordError("zstd decompression failed: the frame's size is not its content")
	}
	return decodeRecord(record, visit)
}

type cursor struct {
	bytes    []byte
	position int
}

func (c *cursor) take(count int) ([]byte, error) {
	if count < 0 || count > len(c.bytes)-c.position {
		return nil, recordError("record ends early")
	}
	slice := c.bytes[c.position : c.position+count]
	c.position += count
	return slice, nil
}

func (c *cursor) u8() (uint8, error) {
	b, err := c.take(1)
	if err != nil {
		return 0, err
	}
	return b[0], nil
}

func (c *cursor) u16() (uint16, error) {
	b, err := c.take(2)
	if err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint16(b), nil
}

func (c *cursor) u32() (uint32, error) {
	b, err := c.take(4)
	if err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint32(b), nil
}

func (c *cursor) u64() (uint64, error) {
	b, err := c.take(8)
	if err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint64(b), nil
}

func (c *cursor) remaining() int {
	return len(c.bytes) - c.position
}

// count reads a count whose items each need at least minimum bytes.
func (c *cursor) count(minimum int) (int, error) {
	value, err := c.u32()
	if err != nil {
		return 0, err
	}
	if uint64(value)*uint64(minimum) > uint64(c.remaining()) {
		return 0, recordError("count exceeds the record")
	}
	return int(value), nil
}

func (c *cursor) text() (string, error) {
	length, err := c.count(1)
	if err != nil {
		return "", err
	}
	b, err := c.take(length)
	if err != nil {
		return "", err
	}
	if !utf8.Valid(b) {
		return "", recordError("text is not UTF-8")
	}
	return string(b), nil
}

// checkJSON checks that text is canonical JSON of an object, or of an
// array when arrayAllowed; an object without arrayAllowed is custom data,
// whose keys are namespaces.
func checkJSON(text string, arrayAllowed bool) error {
	root, err := parseCanonical(text, 1)
	if err != nil {
		return recordError("embedded JSON: %v", err)
	}
	switch root.kind {
	case jsonObject:
		if !arrayAllowed {
			for _, member := range root.children {
				if !isNamespace(member.key) {
					return recordError("custom data must be an object whose keys are namespaces")
				}
			}
		}
		return nil
	case jsonArray:
		if arrayAllowed {
			return nil
		}
		return recordError("custom data must be an object whose keys are namespaces")
	}
	if arrayAllowed {
		return recordError("JSON field values must be arrays or objects")
	}
	return recordError("custom data must be an object whose keys are namespaces")
}

func decodeRecord(record []byte, visit visitor) (*Chunk, error) {
	if len(record) > MaxRecordBytes {
		return nil, recordError("record exceeds 16 MiB")
	}
	c := &cursor{bytes: record}
	magic, err := c.take(4)
	if err != nil {
		return nil, err
	}
	if string(magic) != "TWC1" {
		return nil, recordError("bad magic")
	}
	version, err := c.u16()
	if err != nil {
		return nil, err
	}
	if version != recordVersion {
		return nil, recordError("unsupported version")
	}
	chunkSize, err := c.u16()
	if err != nil {
		return nil, err
	}
	if chunkSize == 0 || chunkSize > maxChunkSize {
		return nil, recordError("chunk size must be 1..=255")
	}
	groundCount, err := c.u8()
	if err != nil {
		return nil, err
	}
	if groundCount > maxGroundCount {
		return nil, recordError("bad ground count or reserved byte")
	}
	reserved, err := c.u8()
	if err != nil {
		return nil, err
	}
	if reserved != 0 {
		return nil, recordError("bad ground count or reserved byte")
	}
	paletteLength, err := c.count(4)
	if err != nil {
		return nil, err
	}
	chunk := &Chunk{ChunkSize: chunkSize, Palette: make([]uint32, paletteLength)}
	for i := range chunk.Palette {
		if chunk.Palette[i], err = c.u32(); err != nil {
			return nil, err
		}
	}
	cells := int(chunkSize) * int(chunkSize)
	chunk.Ground = make([][]uint16, groundCount)
	for g := range chunk.Ground {
		if c.remaining() < cells*2 {
			return nil, recordError("record ends early")
		}
		grid := make([]uint16, cells)
		for i := range grid {
			grid[i], _ = c.u16()
		}
		chunk.Ground[g] = grid
	}
	if c.remaining() < cells*6 {
		return nil, recordError("record ends early")
	}
	chunk.Heights = make([]int32, cells)
	for i := range chunk.Heights {
		height, _ := c.u32()
		chunk.Heights[i] = int32(height)
	}
	chunk.Zones = make([]uint16, cells)
	for i := range chunk.Zones {
		chunk.Zones[i], _ = c.u16()
	}
	if err := checkShape(chunk); err != nil {
		return nil, err
	}
	// Objects are kept only as they decode, never reserved from the count.
	objectCount, err := c.count(minimumObjectBytes)
	if err != nil {
		return nil, err
	}
	for range objectCount {
		object, err := decodeObject(c, cells, 0, visit)
		if err != nil {
			return nil, err
		}
		if visit == nil {
			chunk.Objects = append(chunk.Objects, object)
		}
	}
	if c.remaining() != 0 {
		return nil, recordError("trailing bytes")
	}
	return chunk, nil
}

// checkShape checks the palette and that every grid cell is within it.
func checkShape(chunk *Chunk) error {
	for i, id := range chunk.Palette {
		if id == 0 || (i > 0 && chunk.Palette[i-1] >= id) {
			return recordError("palette string IDs must be nonzero and strictly increasing")
		}
	}
	limit := len(chunk.Palette)
	outside := func(grid []uint16) bool {
		for _, cell := range grid {
			if int(cell) > limit {
				return true
			}
		}
		return false
	}
	for _, grid := range chunk.Ground {
		if outside(grid) {
			return recordError("a grid cell is outside the palette")
		}
	}
	if outside(chunk.Zones) {
		return recordError("a grid cell is outside the palette")
	}
	return nil
}

func decodeObject(c *cursor, cells, depth int, visit visitor) (Object, error) {
	if depth > MaxInventoryDepth {
		return Object{}, recordError("inventory nesting too deep")
	}
	tile, err := c.u16()
	if err != nil {
		return Object{}, err
	}
	switch {
	case depth == 0 && int(tile) < cells:
	case depth > 0 && tile == NoTile:
	default:
		return Object{}, recordError("object tile out of range")
	}
	object := Object{Tile: tile}
	if object.Proto, err = c.u32(); err != nil {
		return Object{}, err
	}
	if object.Layer, err = c.u16(); err != nil {
		return Object{}, err
	}
	subLayer, err := c.u16()
	if err != nil {
		return Object{}, err
	}
	object.SubLayer = int16(subLayer)
	z, err := c.u32()
	if err != nil {
		return Object{}, err
	}
	object.Z = int32(z)
	flags, err := c.u8()
	if err != nil {
		return Object{}, err
	}
	if flags&^(flagPortal|flagFields|flagCustom|flagInventory|flagUID) != 0 {
		return Object{}, recordError("unknown object flags")
	}
	if flags&flagUID != 0 {
		if object.UID, err = decodeUID(c); err != nil {
			return Object{}, err
		}
	}
	if flags&flagPortal != 0 {
		if object.Portal, err = decodePortal(c); err != nil {
			return Object{}, err
		}
	}
	if flags&flagFields != 0 {
		if object.Fields, err = decodeFields(c); err != nil {
			return Object{}, err
		}
	}
	if flags&flagCustom != 0 {
		text, err := c.text()
		if err != nil {
			return Object{}, err
		}
		if err := checkJSON(text, false); err != nil {
			return Object{}, err
		}
		object.Custom = text
	}
	if visit != nil {
		if err := visit(&object); err != nil {
			return Object{}, err
		}
	}
	if flags&flagInventory != 0 {
		count, err := c.count(minimumObjectBytes)
		if err != nil {
			return Object{}, err
		}
		if count == 0 {
			return Object{}, recordError("empty inventory flagged")
		}
		for range count {
			item, err := decodeObject(c, cells, depth+1, visit)
			if err != nil {
				return Object{}, err
			}
			if visit == nil {
				object.Inventory = append(object.Inventory, item)
			}
		}
	}
	return object, nil
}

// decodeUID reads a uid: a length byte, then that many bytes of an
// identifier.
func decodeUID(c *cursor) (string, error) {
	length, err := c.u8()
	if err != nil {
		return "", err
	}
	b, err := c.take(int(length))
	if err != nil {
		return "", err
	}
	if !isIdentifier(string(b)) {
		return "", recordError("a uid is not an identifier")
	}
	return string(b), nil
}

// Object returns the object at index in the chunk's depth-first order,
// where each object comes before its inventory entries and the first
// top-level object is 0: the index an object_uids row holds. It returns
// nil when the chunk has fewer objects.
func (chunk *Chunk) Object(index int) *Object {
	remaining := index
	var find func(objects []Object) *Object
	find = func(objects []Object) *Object {
		for i := range objects {
			if remaining == 0 {
				return &objects[i]
			}
			remaining--
			if found := find(objects[i].Inventory); found != nil {
				return found
			}
		}
		return nil
	}
	if index < 0 {
		return nil
	}
	return find(chunk.Objects)
}

func decodePortal(c *cursor) (*Portal, error) {
	kind, err := c.u8()
	if err != nil {
		return nil, err
	}
	portal := &Portal{Kind: PortalKind(kind)}
	switch portal.Kind {
	case PortalLocation:
		if portal.Plane, err = c.u32(); err != nil {
			return nil, err
		}
		layer, err := c.u32()
		if err != nil {
			return nil, err
		}
		portal.Layer = int32(layer)
		x, err := c.u64()
		if err != nil {
			return nil, err
		}
		y, err := c.u64()
		if err != nil {
			return nil, err
		}
		portal.X, portal.Y = int64(x), int64(y)
	case PortalAnchor:
		if portal.Plane, err = c.u32(); err != nil {
			return nil, err
		}
		if portal.Anchor, err = c.u32(); err != nil {
			return nil, err
		}
	default:
		return nil, recordError("unknown portal kind")
	}
	return portal, nil
}

func decodeFields(c *cursor) ([]Field, error) {
	count16, err := c.u16()
	if err != nil {
		return nil, err
	}
	count := int(count16)
	if count == 0 || count*5 > c.remaining() {
		return nil, recordError("bad field count")
	}
	// Fields are kept only as they decode, never reserved from the count.
	var fields []Field
	for range count {
		var field Field
		if field.Name, err = c.u32(); err != nil {
			return nil, err
		}
		tag, err := c.u8()
		if err != nil {
			return nil, err
		}
		switch tag {
		case 0:
			field.Kind = FieldNull
		case 1, 2:
			field.Kind, field.Bool = FieldBool, tag == 2
		case 3:
			field.Kind = FieldInteger
			value, err := c.u64()
			if err != nil {
				return nil, err
			}
			field.Integer = int64(value)
		case 4:
			field.Kind = FieldNumber
			bits, err := c.u64()
			if err != nil {
				return nil, err
			}
			field.Number = math.Float64frombits(bits)
			if math.IsNaN(field.Number) || math.IsInf(field.Number, 0) {
				return nil, recordError("numbers must be finite")
			}
		case 5:
			field.Kind = FieldString
			if field.String, err = c.u32(); err != nil {
				return nil, err
			}
		case 6:
			field.Kind = FieldJSON
			text, err := c.text()
			if err != nil {
				return nil, err
			}
			if err := checkJSON(text, true); err != nil {
				return nil, err
			}
			field.JSON = text
		default:
			return nil, recordError("unknown field tag")
		}
		fields = append(fields, field)
	}
	return fields, nil
}

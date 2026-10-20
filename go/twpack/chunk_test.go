// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"encoding/binary"
	"slices"
	"strings"
	"testing"

	"github.com/klauspost/compress/zstd"
)

func TestNamesMatchTheSourcePatterns(t *testing.T) {
	for text, expected := range map[string]bool{
		"a": true, "0": true, "a.b-c_d": true, strings.Repeat("a", 128): true,
		"": false, "A": false, ".a": false, "_a": false, "a b": false, "a/b": false,
		strings.Repeat("a", 129): false, "é": false, "a\x00": false,
	} {
		if isIdentifier(text) != expected {
			t.Errorf("isIdentifier(%q) != %v", text, expected)
		}
	}
	for text, expected := range map[string]bool{
		"example.org": true, "example.org/tool": true, "a.b/": true, "a.b/c.d": true,
		"example": false, "example.": false, ".example.org": false, "example.org/a/b": false,
		"Example.org": false, "example.org/Tool": false,
	} {
		if isNamespace(text) != expected {
			t.Errorf("isNamespace(%q) != %v", text, expected)
		}
	}
	for text, expected := range map[string]bool{
		"a": true, "a_1": true, strings.Repeat("a", 64): true,
		"": false, "1a": false, "_a": false, "a.b": false, "a-b": false, strings.Repeat("a", 65): false,
	} {
		if isFieldName(text) != expected {
			t.Errorf("isFieldName(%q) != %v", text, expected)
		}
	}
}

// testRecord is a 2 × 2 record with one ground layer and the given
// objects.
func testRecord(objects ...[]byte) []byte {
	record := []byte("TWC1")
	record = binary.LittleEndian.AppendUint16(record, 1)
	record = binary.LittleEndian.AppendUint16(record, 2)
	record = append(record, 1, 0)
	record = binary.LittleEndian.AppendUint32(record, 1)
	record = binary.LittleEndian.AppendUint32(record, 7)
	record = append(record, 1, 0, 0, 0, 1, 0, 0, 0)
	record = append(record, make([]byte, 4*4+4*2)...)
	record = binary.LittleEndian.AppendUint32(record, uint32(len(objects)))
	for _, object := range objects {
		record = append(record, object...)
	}
	return record
}

func testObject(tile uint16, proto uint32, flags byte, rest ...byte) []byte {
	object := binary.LittleEndian.AppendUint16(nil, tile)
	object = binary.LittleEndian.AppendUint32(object, proto)
	object = append(object, 3, 0, 0xFF, 0xFF, 9, 0, 0, 0, flags)
	return append(object, rest...)
}

func TestRecordsDecodeAndStreamObjectsInOrder(t *testing.T) {
	inventory := binary.LittleEndian.AppendUint32(nil, 2)
	inventory = append(inventory, testObject(NoTile, 2, 0)...)
	inventory = append(inventory, testObject(NoTile, 3, 0)...)
	record := testRecord(testObject(3, 1, flagInventory, inventory...), testObject(0, 4, 0))
	chunk, err := DecodeRecord(record)
	if err != nil {
		t.Fatal(err)
	}
	if chunk.ChunkSize != 2 || !slices.Equal(chunk.Palette, []uint32{7}) || !slices.Equal(chunk.Ground[0], []uint16{1, 0, 1, 0}) {
		t.Fatalf("%+v", chunk)
	}
	first := chunk.Objects[0]
	if first.Tile != 3 || first.Layer != 3 || first.SubLayer != -1 || first.Z != 9 || len(first.Inventory) != 2 || first.Inventory[1].Proto != 3 {
		t.Fatalf("%+v", first)
	}
	var visited []uint32
	streamed, err := decodeRecord(record, func(object *Object) error {
		if object.Inventory != nil {
			t.Error("a visited object holds its inventory")
		}
		visited = append(visited, object.Proto)
		return nil
	})
	if err != nil || streamed.Objects != nil || !slices.Equal(visited, []uint32{1, 2, 3, 4}) {
		t.Fatalf("%v %v %v", streamed, visited, err)
	}
	compressed, err := zstd.NewWriter(nil, zstd.WithSingleSegment(true))
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := DecodeChunk(compressed.EncodeAll(record, nil))
	if err != nil || len(decoded.Objects) != 2 {
		t.Fatal(decoded, err)
	}
	_, err = DecodeChunk(make([]byte, MaxChunkBytes+1))
	expectCode(t, "oversized chunk data", err, CodeChunkRecord)
	_, err = DecodeRecord(make([]byte, MaxRecordBytes+1))
	expectCode(t, "an oversized record", err, CodeChunkRecord)
}

func TestFrameHeadersAreReadAsZstdReadsThem(t *testing.T) {
	magic := binary.LittleEndian.AppendUint32(nil, zstdMagic)
	for _, c := range []struct {
		name   string
		data   []byte
		ok     bool
		size   uint64
		hasFCS bool
	}{
		{"single segment, 1-byte size", append(slices.Clone(magic), 0x20, 200), true, 200, true},
		{"2-byte size adds 256", append(slices.Clone(magic), 0x60, 1, 0), true, 257, true},
		{"4-byte size with a window", append(slices.Clone(magic), 0x80, 0x50, 4, 3, 2, 1), true, 0x01020304, true},
		{"no size", append(slices.Clone(magic), 0x00, 0x50), true, 0, false},
		{"reserved bit", append(slices.Clone(magic), 0x28, 1), false, 0, false},
		{"window past 2^31", append(slices.Clone(magic), 0x00, 22<<3), false, 0, false},
		{"truncated", append(slices.Clone(magic), 0x80, 0x50, 4), false, 0, false},
		{"other magic", []byte{1, 2, 3, 4, 0x20, 1}, false, 0, false},
	} {
		header, ok := parseFrameHeader(c.data)
		if ok != c.ok || (ok && (header.contentSize != c.size || header.hasSize != c.hasFCS)) {
			t.Errorf("%s: %+v %v", c.name, header, ok)
		}
	}
	skippable := binary.LittleEndian.AppendUint32(nil, skippableMagic+3)
	skippable = binary.LittleEndian.AppendUint32(skippable, 2)
	header, ok := parseFrameHeader(append(skippable, 0, 0))
	if !ok || !header.skippable || recordedSize(append(skippable, 0, 0)) != 0 {
		t.Fatalf("%+v %v", header, ok)
	}
	if length, ok := frameLength(append(skippable, 0, 0), header); !ok || length != 10 {
		t.Fatal(length, ok)
	}
	if _, ok := frameLength(append(skippable, 0), header); ok {
		t.Fatal("a skippable frame past its data was measured")
	}
}

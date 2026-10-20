// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"errors"
	"strconv"
	"unicode/utf8"
)

// The source format's JSON limits (docs/spec/FORMAT.md section 3), which
// every JSON text stored in a pack already met as source.
const (
	maxDocumentBytes = 64 << 20
	maxDepth         = 64
	maxValues        = 1_000_000
	maxArrayItems    = 250_000
	maxObjectKeys    = 256
	maxStringBytes   = 1 << 20
	maxNumberBytes   = 128
	// MaxSafeInteger is the largest integer magnitude the format accepts,
	// 2^53 - 1.
	MaxSafeInteger = 9_007_199_254_740_991
)

type jsonKind uint8

const (
	jsonNull jsonKind = iota
	jsonBool
	jsonInteger
	jsonFloat
	jsonString
	jsonArray
	jsonObject
)

// jsonValue is a parsed value. Containers keep their children only down to
// the depth the parse asked for; deeper ones are checked but not kept.
type jsonValue struct {
	kind    jsonKind
	boolean bool
	integer int64
	// text is a string's value.
	text string
	// key is the member's key, for a child of an object.
	key string
	// children are an array's items or an object's members.
	children []jsonValue
	// start and end are the value's byte span in the source.
	start, end int
}

// get returns the member key of an object.
func (v *jsonValue) get(key string) (*jsonValue, bool) {
	if v.kind != jsonObject {
		return nil, false
	}
	for i := range v.children {
		if v.children[i].key == key {
			return &v.children[i], true
		}
	}
	return nil, false
}

// str returns the member key of an object when it is a string.
func (v *jsonValue) str(key string) (string, bool) {
	member, ok := v.get(key)
	if !ok || member.kind != jsonString {
		return "", false
	}
	return member.text, true
}

// errNotCanonical is returned for text that is not canonical JSON within
// the format's limits.
var errNotCanonical = errors.New("not canonical JSON within the format's limits")

// parseCanonical parses text that must be canonical JSON as the toolkit
// writes it: no whitespace or comments, object members in any order without
// duplicates, numbers spelled as any valid JSON number (integers within
// ±MaxSafeInteger, finite floats), and strings escaped only where JSON
// requires (", \ and control characters, with \b, \f, \n, \r and \t
// short, other control characters as lowercase \u00xx, and no NUL). It
// enforces the source format's limits while reading. Containers keep their
// children down to depth keep, where the root container is depth 1.
func parseCanonical(text string, keep int) (jsonValue, error) {
	if len(text) > maxDocumentBytes || !utf8.ValidString(text) {
		return jsonValue{}, errNotCanonical
	}
	parser := canonicalParser{source: text, keep: keep}
	root, ok := parser.value(0)
	if !ok || parser.position != len(text) {
		return jsonValue{}, errNotCanonical
	}
	return root, nil
}

type canonicalParser struct {
	source   string
	position int
	values   int
	keep     int
}

func (p *canonicalParser) peek() (byte, bool) {
	if p.position < len(p.source) {
		return p.source[p.position], true
	}
	return 0, false
}

func (p *canonicalParser) value(depth int) (jsonValue, bool) {
	p.values++
	if p.values > maxValues {
		return jsonValue{}, false
	}
	start := p.position
	c, ok := p.peek()
	if !ok {
		return jsonValue{}, false
	}
	switch {
	case c == '{':
		return p.object(depth + 1)
	case c == '[':
		return p.array(depth + 1)
	case c == '"':
		text, ok := p.string()
		return jsonValue{kind: jsonString, text: text, start: start, end: p.position}, ok
	case c == 't':
		return p.literal("true", jsonValue{kind: jsonBool, boolean: true})
	case c == 'f':
		return p.literal("false", jsonValue{kind: jsonBool})
	case c == 'n':
		return p.literal("null", jsonValue{kind: jsonNull})
	case c == '-' || ('0' <= c && c <= '9'):
		return p.number()
	}
	return jsonValue{}, false
}

func (p *canonicalParser) literal(word string, value jsonValue) (jsonValue, bool) {
	if len(p.source)-p.position < len(word) || p.source[p.position:p.position+len(word)] != word {
		return jsonValue{}, false
	}
	value.start = p.position
	p.position += len(word)
	value.end = p.position
	return value, true
}

func isDigit(c byte) bool {
	return '0' <= c && c <= '9'
}

func (p *canonicalParser) digitAt(index int) bool {
	return index < len(p.source) && isDigit(p.source[index])
}

func (p *canonicalParser) number() (jsonValue, bool) {
	start := p.position
	cursor := start
	if p.source[cursor] == '-' {
		cursor++
	}
	switch {
	case cursor < len(p.source) && p.source[cursor] == '0':
		cursor++
	case p.digitAt(cursor):
		for p.digitAt(cursor) {
			cursor++
		}
	default:
		return jsonValue{}, false
	}
	integer := true
	if cursor < len(p.source) && p.source[cursor] == '.' {
		integer = false
		cursor++
		if !p.digitAt(cursor) {
			return jsonValue{}, false
		}
		for p.digitAt(cursor) {
			cursor++
		}
	}
	if cursor < len(p.source) && (p.source[cursor] == 'e' || p.source[cursor] == 'E') {
		integer = false
		cursor++
		if cursor < len(p.source) && (p.source[cursor] == '+' || p.source[cursor] == '-') {
			cursor++
		}
		if !p.digitAt(cursor) {
			return jsonValue{}, false
		}
		for p.digitAt(cursor) {
			cursor++
		}
	}
	if cursor-start > maxNumberBytes {
		return jsonValue{}, false
	}
	text := p.source[start:cursor]
	p.position = cursor
	if integer {
		value, err := strconv.ParseInt(text, 10, 64)
		if err != nil || value < -MaxSafeInteger || value > MaxSafeInteger {
			return jsonValue{}, false
		}
		return jsonValue{kind: jsonInteger, integer: value, start: start, end: cursor}, true
	}
	// Underflow rounds to zero, as it does for the Rust reader; overflow
	// is not finite.
	value, err := strconv.ParseFloat(text, 64)
	if err != nil {
		var numberError *strconv.NumError
		if !errors.As(err, &numberError) || numberError.Err != strconv.ErrRange || value > 1 || value < -1 {
			return jsonValue{}, false
		}
	}
	return jsonValue{kind: jsonFloat, start: start, end: cursor}, true
}

// string reads a canonical string, which escapes only ", \ and control
// characters, exactly as the toolkit writes them.
func (p *canonicalParser) string() (string, bool) {
	p.position++
	runStart := p.position
	var decoded []byte
	escaped := false
	for {
		c, ok := p.peek()
		if !ok {
			return "", false
		}
		switch {
		case c == '"':
			var text string
			if escaped {
				decoded = append(decoded, p.source[runStart:p.position]...)
				text = string(decoded)
			} else {
				text = p.source[runStart:p.position]
			}
			p.position++
			return text, len(text) <= maxStringBytes
		case c < 0x20:
			return "", false
		case c == '\\':
			if !escaped {
				escaped = true
				decoded = make([]byte, 0, p.position-runStart+16)
			}
			decoded = append(decoded, p.source[runStart:p.position]...)
			character, ok := p.escape()
			if !ok {
				return "", false
			}
			decoded = append(decoded, character)
			runStart = p.position
		default:
			p.position++
		}
		if p.position-runStart+len(decoded) > maxStringBytes {
			return "", false
		}
	}
}

// escape reads one escape sequence, which must be the one the toolkit
// writes for the character it stands for.
func (p *canonicalParser) escape() (byte, bool) {
	if p.position+1 >= len(p.source) {
		return 0, false
	}
	kind := p.source[p.position+1]
	p.position += 2
	switch kind {
	case '"':
		return '"', true
	case '\\':
		return '\\', true
	case 'b':
		return 0x08, true
	case 'f':
		return 0x0c, true
	case 'n':
		return '\n', true
	case 'r':
		return '\r', true
	case 't':
		return '\t', true
	case 'u':
		// Only control characters without a short escape, other than NUL,
		// as \u00 and two lowercase hex digits.
		if len(p.source)-p.position < 4 || p.source[p.position:p.position+2] != "00" {
			return 0, false
		}
		high, low := p.source[p.position+2], p.source[p.position+3]
		p.position += 4
		if high != '0' && high != '1' {
			return 0, false
		}
		var nibble byte
		switch {
		case '0' <= low && low <= '9':
			nibble = low - '0'
		case 'a' <= low && low <= 'f':
			nibble = low - 'a' + 10
		default:
			return 0, false
		}
		character := (high-'0')<<4 | nibble
		switch character {
		case 0, 0x08, 0x09, 0x0a, 0x0c, 0x0d:
			return 0, false
		}
		return character, true
	}
	return 0, false
}

func (p *canonicalParser) array(depth int) (jsonValue, bool) {
	start := p.position
	if depth > maxDepth {
		return jsonValue{}, false
	}
	p.position++
	value := jsonValue{kind: jsonArray, start: start}
	if c, ok := p.peek(); ok && c == ']' {
		p.position++
		value.end = p.position
		return value, true
	}
	count := 0
	for {
		if count == maxArrayItems {
			return jsonValue{}, false
		}
		item, ok := p.value(depth)
		if !ok {
			return jsonValue{}, false
		}
		count++
		if depth <= p.keep {
			value.children = append(value.children, item)
		}
		c, ok := p.peek()
		switch {
		case ok && c == ',':
			p.position++
		case ok && c == ']':
			p.position++
			value.end = p.position
			return value, true
		default:
			return jsonValue{}, false
		}
	}
}

func (p *canonicalParser) object(depth int) (jsonValue, bool) {
	start := p.position
	if depth > maxDepth {
		return jsonValue{}, false
	}
	p.position++
	value := jsonValue{kind: jsonObject, start: start}
	if c, ok := p.peek(); ok && c == '}' {
		p.position++
		value.end = p.position
		return value, true
	}
	var keys []string
	var seen map[string]struct{}
	for {
		if c, ok := p.peek(); !ok || c != '"' || len(keys) == maxObjectKeys {
			return jsonValue{}, false
		}
		key, ok := p.string()
		if !ok {
			return jsonValue{}, false
		}
		if seen != nil {
			if _, duplicate := seen[key]; duplicate {
				return jsonValue{}, false
			}
			seen[key] = struct{}{}
		} else {
			for _, other := range keys {
				if other == key {
					return jsonValue{}, false
				}
			}
			if len(keys) == 16 {
				seen = make(map[string]struct{}, maxObjectKeys)
				for _, other := range keys {
					seen[other] = struct{}{}
				}
				seen[key] = struct{}{}
			}
		}
		keys = append(keys, key)
		if c, ok := p.peek(); !ok || c != ':' {
			return jsonValue{}, false
		}
		p.position++
		member, ok := p.value(depth)
		if !ok {
			return jsonValue{}, false
		}
		if depth <= p.keep {
			member.key = key
			value.children = append(value.children, member)
		}
		c, ok := p.peek()
		switch {
		case ok && c == ',':
			p.position++
		case ok && c == '}':
			p.position++
			value.end = p.position
			return value, true
		default:
			return jsonValue{}, false
		}
	}
}

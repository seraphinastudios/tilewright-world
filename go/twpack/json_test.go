// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"fmt"
	"strings"
	"testing"
)

func TestCanonicalJSONIsAcceptedExactly(t *testing.T) {
	for _, text := range []string{
		`null`, `true`, `false`, `0`, `-0`, `9007199254740991`, `-9007199254740991`,
		`1.5`, `-0.0`, `1E+2`, `2.5e-3`, `1e-400`,
		`""`, `"é"`, "\"\u2028\x7f\"", `"\"\\\b\f\n\r\t"`, `"\u0001\u001f\u000b"`,
		`[]`, `{}`, `[1,[2,[]],{"a":{}}]`, `{"a":1,"b":[true,null]}`,
	} {
		if _, err := parseCanonical(text, maxDepth); err != nil {
			t.Errorf("%q: %v", text, err)
		}
	}
}

func TestNonCanonicalOrInvalidJSONIsRejected(t *testing.T) {
	for _, text := range []string{
		``, ` 1`, `1 `, `[1, 2]`, `{"a": 1}`, "[1,\n2]", `[1,]`, `{"a":1,}`, `[1/*x*/]`, `//x` + "\n1",
		`{"a":1,"a":2}`, `{a:1}`, `[01]`, `[1.]`, `[.5]`, `[1e]`, `[+1]`, `[0x1]`, `[NaN]`, `[1e400]`,
		`[9007199254740992]`, `[-9007199254740992]`, `[99999999999999999999]`,
		`["\/"]`, `["\u0041"]`, `["\u00e9"]`, `["\u001F"]`, `["\u000a"]`, `["\u0008"]`, `["\u0000"]`,
		`["\ud83d\ude00"]`, `["\uD83D"]`, `["\x"]`, "[\"\t\"]", "[\"\x00\"]", "[\"\xff\"]", "\ufeff[1]", `"abc`, `tru`, `nul`,
		`[1]]`, `{"a"}`, `{"a":}`, `{,}`, `[,]`,
	} {
		if _, err := parseCanonical(text, maxDepth); err == nil {
			t.Errorf("%q was accepted", text)
		}
	}
}

func TestCanonicalJSONLimits(t *testing.T) {
	cases := []struct {
		name     string
		at, past string
	}{
		{"depth", strings.Repeat("[", 64) + strings.Repeat("]", 64), strings.Repeat("[", 65) + strings.Repeat("]", 65)},
		{"number", "[" + strings.Repeat("1", 15) + "." + strings.Repeat("0", 112) + "]", "[" + strings.Repeat("1", 15) + "." + strings.Repeat("0", 113) + "]"},
		{"string", `"` + strings.Repeat("x", 1<<20) + `"`, `"` + strings.Repeat("x", 1<<20+1) + `"`},
		{"escaped string", `"` + strings.Repeat("x", 1<<20-1) + `\n"`, `"` + strings.Repeat("x", 1<<20) + `\n"`},
		{"array", "[" + strings.Repeat("0,", maxArrayItems-1) + "0]", "[" + strings.Repeat("0,", maxArrayItems) + "0]"},
		// The root, 199,999 arrays of four values, and four more.
		{"values", "[" + strings.Repeat("[0,0,0,0],", 199_999) + "0,0,0,0]", "[" + strings.Repeat("[0,0,0,0],", 199_999) + "0,0,0,0,0]"},
	}
	keys := func(count int) string {
		members := make([]string, count)
		for i := range members {
			members[i] = fmt.Sprintf(`"k%d":0`, i)
		}
		return "{" + strings.Join(members, ",") + "}"
	}
	cases = append(cases, struct {
		name     string
		at, past string
	}{"object", keys(maxObjectKeys), keys(maxObjectKeys + 1)})
	for _, c := range cases {
		if _, err := parseCanonical(c.at, maxDepth); err != nil {
			t.Errorf("%s: at the limit: %v", c.name, err)
		}
		if _, err := parseCanonical(c.past, maxDepth); err == nil {
			t.Errorf("%s: past the limit was accepted", c.name)
		}
	}
	if _, err := parseCanonical(`"`+strings.Repeat("x", maxDocumentBytes)+`"`, maxDepth); err == nil {
		t.Error("a document past 64 MiB was accepted")
	}
}

func TestParsedValuesKeepTheRequestedDepth(t *testing.T) {
	root, err := parseCanonical(`{"a":{"b":[1]},"c":"d"}`, 1)
	if err != nil {
		t.Fatal(err)
	}
	a, ok := root.get("a")
	if !ok || a.kind != jsonObject || a.children != nil || a.start != 5 || a.end != 14 {
		t.Fatalf("%+v", a)
	}
	if c, ok := root.str("c"); !ok || c != "d" {
		t.Fatal(c, ok)
	}
	root, err = parseCanonical(`{"a":{"b":[-5]}}`, maxDepth)
	if err != nil {
		t.Fatal(err)
	}
	a, _ = root.get("a")
	b, _ := a.get("b")
	if len(b.children) != 1 || b.children[0].kind != jsonInteger || b.children[0].integer != -5 {
		t.Fatalf("%+v", b)
	}
}

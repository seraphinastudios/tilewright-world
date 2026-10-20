// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import "strings"

func isLowerOrDigit(c byte) bool {
	return ('a' <= c && c <= 'z') || ('0' <= c && c <= '9')
}

// isIdentifier reports whether text matches ^[a-z0-9][a-z0-9_.-]{0,127}$,
// the source format's identifier pattern.
func isIdentifier(text string) bool {
	if len(text) < 1 || len(text) > 128 || !isLowerOrDigit(text[0]) {
		return false
	}
	for i := 1; i < len(text); i++ {
		c := text[i]
		if !isLowerOrDigit(c) && c != '_' && c != '.' && c != '-' {
			return false
		}
	}
	return true
}

// isNamespace reports whether text is a domain-qualified namespace for
// custom data, such as example.org or example.org/tool.
func isNamespace(text string) bool {
	domain, rest, _ := strings.Cut(text, "/")
	return strings.Contains(domain, ".") &&
		isIdentifier(domain) &&
		!strings.HasPrefix(domain, ".") &&
		!strings.HasSuffix(domain, ".") &&
		(rest == "" || isIdentifier(rest))
}

// isFieldName reports whether text matches ^[a-z][a-z0-9_]{0,63}$.
func isFieldName(text string) bool {
	if len(text) < 1 || len(text) > 64 || text[0] < 'a' || text[0] > 'z' {
		return false
	}
	for i := 1; i < len(text); i++ {
		c := text[i]
		if !isLowerOrDigit(c) && c != '_' {
			return false
		}
	}
	return true
}

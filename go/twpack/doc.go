// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

// Package twpack reads Tilewright content packs (docs/spec/PACK.md in
// github.com/seraphinastudios/tilewright-world): one SQLite database file
// compiled from a world source tree and named by its SHA-256 digest.
//
// Packs are untrusted input. Open opens the file read-only and immutable,
// in SQLite's defensive mode with untrusted schemas disabled and queries
// only, and checks its size, schema, identity, free pages, length and meta
// before any other query runs. Every read checks a value's type and size in
// SQL before loading it, decodes chunk records within the twc1+zstd bounds,
// and never loads a whole table into memory. Verify checks the whole pack:
// SQLite integrity, the file name digest, every row and stored document,
// and every target a chunk record names.
//
//	pack, err := twpack.Open(ctx, "dist/<sha256>.twpack", twpack.Limits{})
//	if err != nil {
//		return err
//	}
//	defer pack.Close()
//	if _, err := pack.Verify(ctx, twpack.Limits{}); err != nil {
//		return err
//	}
//	chunk, ok, err := pack.Chunk(ctx, "overworld", 0, 0, 0)
//
// Errors carry a stable Code, shared with the Rust reader; CodeOf returns
// it. Errors from a canceled or expired context are the context's own.
package twpack

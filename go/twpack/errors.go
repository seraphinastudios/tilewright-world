// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"context"
	"errors"
)

// Code is a stable reason for rejecting a pack, one per rule of
// docs/spec/PACK.md section 3. The Rust reader reports the same codes, and
// the conformance fixtures in fixtures/packs pin them.
type Code string

// The codes a reader reports.
const (
	// CodeIO: the file could not be read.
	CodeIO Code = "pack.io"
	// CodeSQLite: SQLite failed, for example on a file that is not a
	// database.
	CodeSQLite Code = "pack.sqlite"
	// CodeNotAFile: the path is not a regular file.
	CodeNotAFile Code = "pack.not_a_file"
	// CodeTooLarge: the file is longer than the pack size limit.
	CodeTooLarge Code = "pack.too_large"
	// CodeFileLength: the file is not exactly its pages.
	CodeFileLength Code = "pack.file_length"
	// CodeSchema: the schema is not exactly the format's.
	CodeSchema Code = "pack.schema"
	// CodeIdentity: the application ID, user version or page size is not
	// the format's.
	CodeIdentity Code = "pack.identity"
	// CodeFreePages: the pack has free pages.
	CodeFreePages Code = "pack.free_pages"
	// CodeMeta: meta does not hold exactly the format's keys and values.
	CodeMeta Code = "pack.meta"
	// CodeProfile: the stored profile is invalid or disagrees with meta.
	CodeProfile Code = "pack.profile"
	// CodeString: a string is not UTF-8 text of at most 1 MiB without NUL.
	CodeString Code = "pack.string"
	// CodeStringIDs: string IDs are not 1..count in byte order of their
	// values.
	CodeStringIDs Code = "pack.string_ids"
	// CodePlane: a plane row or document is invalid.
	CodePlane Code = "pack.plane"
	// CodeZone: a zone row or document is invalid, or zone parents form a
	// cycle.
	CodeZone Code = "pack.zone"
	// CodeAnchor: an anchor row is invalid.
	CodeAnchor Code = "pack.anchor"
	// CodeStairwell: a stairwell row or document is invalid or disagrees
	// with its steps.
	CodeStairwell Code = "pack.stairwell"
	// CodeStairwellStep: a stairwell_steps row is invalid or listed by no
	// stairwell.
	CodeStairwellStep Code = "pack.stairwell_step"
	// CodeDefinition: a definition row or document is invalid.
	CodeDefinition Code = "pack.definition"
	// CodeChunk: a chunk row's key is invalid, or its record disagrees
	// with its plane or the profile.
	CodeChunk Code = "pack.chunk"
	// CodeChunkEncoding: a chunk's encoding is not twc1+zstd.
	CodeChunkEncoding Code = "pack.chunk_encoding"
	// CodeChunkData: a chunk's data is not a blob within the encoding's
	// bound.
	CodeChunkData Code = "pack.chunk_data"
	// CodeChunkRecord: a chunk's data is not a valid twc1+zstd record.
	CodeChunkRecord Code = "pack.chunk_record"
	// CodeReference: a chunk record names a target that does not exist or
	// is out of range.
	CodeReference Code = "pack.reference"
	// CodeObjectUID: an object_uids row is invalid, or the table does not
	// index exactly the uids of the chunk records.
	CodeObjectUID Code = "pack.object_uid"
	// CodeIntegrity: SQLite's integrity check failed.
	CodeIntegrity Code = "pack.integrity"
	// CodeDigest: the file's SHA-256 does not match its name.
	CodeDigest Code = "pack.digest"
	// CodeDecodeBudget: the chunks decode to more than the verification
	// budget.
	CodeDecodeBudget Code = "pack.decode_budget"
)

// Codes lists every code a reader reports, in the order above.
func Codes() []Code {
	return []Code{
		CodeIO, CodeSQLite, CodeNotAFile, CodeTooLarge, CodeFileLength,
		CodeSchema, CodeIdentity, CodeFreePages, CodeMeta, CodeProfile,
		CodeString, CodeStringIDs, CodePlane, CodeZone, CodeAnchor,
		CodeStairwell, CodeStairwellStep, CodeDefinition, CodeChunk,
		CodeChunkEncoding, CodeChunkData, CodeChunkRecord, CodeReference,
		CodeObjectUID, CodeIntegrity, CodeDigest, CodeDecodeBudget,
	}
}

// Error is a pack that cannot be opened, read or verified, with the code of
// the rule it breaks. Err, when set, is the underlying I/O or SQLite error.
type Error struct {
	Code    Code
	Message string
	Err     error
}

func (e *Error) Error() string {
	if e.Err != nil {
		return e.Message + ": " + e.Err.Error()
	}
	return e.Message
}

func (e *Error) Unwrap() error {
	return e.Err
}

// CodeOf returns the code of the first *Error in err's chain, or "" when
// there is none, as for a canceled context.
func CodeOf(err error) Code {
	var packError *Error
	if errors.As(err, &packError) {
		return packError.Code
	}
	return ""
}

func reject(code Code, message string) error {
	return &Error{Code: code, Message: message}
}

// sqliteError wraps an error from SQLite. A canceled or expired context is
// not a problem with the pack, so its error is returned as it is.
func sqliteError(ctx context.Context, err error) error {
	if ctxErr := ctx.Err(); ctxErr != nil {
		return ctxErr
	}
	var packError *Error
	if errors.As(err, &packError) {
		return err
	}
	return &Error{Code: CodeSQLite, Message: "SQLite error", Err: err}
}

func ioError(err error) error {
	return &Error{Code: CodeIO, Message: "I/O error", Err: err}
}

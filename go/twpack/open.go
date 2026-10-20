// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"

	// The pure-Go SQLite driver, registered as "sqlite".
	_ "modernc.org/sqlite"
)

// Pack format constants (docs/spec/PACK.md).
const (
	// Format is the meta.format value.
	Format = "tilewright.pack/1"
	// ApplicationID is the SQLite application_id of a pack: "TWP1".
	ApplicationID = 0x54575031
	// UserVersion is the SQLite user_version of a pack of this format.
	UserVersion = 1
	// PageSize is the page size of every pack.
	PageSize = 4096
	// DefaultMaxPackBytes is the default Limits.MaxPackBytes: 8 GiB.
	DefaultMaxPackBytes = 8 << 30
	// DefaultMaxDecodedBytes is the default Limits.MaxDecodedBytes: 8 GiB.
	DefaultMaxDecodedBytes = 8 << 30
)

// Largest meta value other than profile, and largest sqlite_schema value
// loaded; every expected one is shorter.
const (
	maxMetaBytes   = 1024
	maxSchemaBytes = 1024
)

// metaKeys lists every meta key, in byte order. A pack has exactly these.
var metaKeys = []string{
	"chunk_encoding",
	"compiler",
	"format",
	"profile",
	"profile_id",
	"profile_version",
	"source_digest",
}

// schemaTables are the pack tables, in name order, exactly as created.
var schemaTables = [][2]string{
	{"anchors", "CREATE TABLE anchors (plane TEXT NOT NULL, id TEXT NOT NULL, layer INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID"},
	{"chunks", "CREATE TABLE chunks (plane TEXT NOT NULL, layer INTEGER NOT NULL, cx INTEGER NOT NULL, cy INTEGER NOT NULL, encoding TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (plane, layer, cx, cy)) WITHOUT ROWID"},
	{"definitions", "CREATE TABLE definitions (type TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY (type, id)) WITHOUT ROWID"},
	{"meta", "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID"},
	{"planes", "CREATE TABLE planes (id TEXT PRIMARY KEY, kind TEXT NOT NULL, chunk_size INTEGER NOT NULL, document TEXT NOT NULL) WITHOUT ROWID"},
	{"stairwell_steps", "CREATE TABLE stairwell_steps (plane TEXT NOT NULL, layer INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL, stairwell TEXT NOT NULL, step INTEGER NOT NULL, proto TEXT NOT NULL, PRIMARY KEY (plane, layer, x, y)) WITHOUT ROWID"},
	{"stairwells", "CREATE TABLE stairwells (plane TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID"},
	{"strings", "CREATE TABLE strings (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE)"},
	{"zones", "CREATE TABLE zones (plane TEXT NOT NULL, id TEXT NOT NULL, parent TEXT, document TEXT NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID"},
}

// Limits bounds the work opening and verifying a pack may do. A zero field
// takes its default.
type Limits struct {
	// MaxPackBytes is the longest pack file opened or hashed. Open checks
	// the file's length before SQLite opens it and its pages once it is
	// open; verification checks the pages again before its integrity check,
	// and hashing fails once it has read more, even from a file that has
	// grown since it was opened. Verification uses the lower of this and
	// the limit the pack was opened with.
	MaxPackBytes uint64
	// MaxDecodedBytes is the most bytes of chunk records verification
	// decodes, summed over every chunk. Verification fails once the chunks'
	// recorded sizes pass it, before the chunk that would pass it is
	// decompressed.
	MaxDecodedBytes uint64
}

func (l Limits) withDefaults() Limits {
	if l.MaxPackBytes == 0 {
		l.MaxPackBytes = DefaultMaxPackBytes
	}
	if l.MaxDecodedBytes == 0 {
		l.MaxDecodedBytes = DefaultMaxDecodedBytes
	}
	return l
}

// Pack is an open content pack. Its methods may be called from several
// goroutines; they share one read-only SQLite connection, which serializes
// them.
type Pack struct {
	db   *sql.DB
	conn *sql.Conn
	path string
	// maxBytes is the pack size limit it was opened with, which bounds
	// FileDigest and Verify.
	maxBytes uint64
}

func tooLarge(maximum uint64) error {
	return reject(CodeTooLarge, fmt.Sprintf("the pack is more than %d bytes, the pack size limit", maximum))
}

// uri percent-encodes an absolute path as an SQLite URI that opens it
// read-only and immutable, in defensive mode, with untrusted schemas
// disabled and queries only. On Unix the path's bytes are encoded as they
// are.
func uri(absolute string) string {
	var encoded strings.Builder
	if runtime.GOOS == "windows" {
		// file:///C:/path; SQLite accepts forward slashes.
		encoded.WriteString("file:///")
		absolute = filepath.ToSlash(absolute)
	} else {
		// file:// and then the absolute path, so a path that itself begins
		// with // stays part of the file name instead of becoming an
		// authority.
		encoded.WriteString("file://")
	}
	for i := 0; i < len(absolute); i++ {
		c := absolute[i]
		if isLowerOrDigit(c) || ('A' <= c && c <= 'Z') || strings.IndexByte("/-_.~", c) >= 0 ||
			(runtime.GOOS == "windows" && c == ':') {
			encoded.WriteByte(c)
		} else {
			fmt.Fprintf(&encoded, "%%%02X", c)
		}
	}
	// mode and immutable are SQLite's; the rest are the driver's, which
	// applies _defensive before anything runs on the connection.
	encoded.WriteString("?mode=ro&immutable=1&_defensive=1&_pragma=trusted_schema(0)&_query_only=1")
	return encoded.String()
}

// Open opens a pack and checks its size, identity and schema before any
// other query runs. A file longer than limits.MaxPackBytes is rejected
// before SQLite opens it, and FileDigest reads at most that many bytes.
// limits.MaxDecodedBytes is not used.
func Open(ctx context.Context, path string, limits Limits) (*Pack, error) {
	limits = limits.withDefaults()
	// The same absolute path opens the database and is hashed later, so a
	// change of working directory cannot point them at different files.
	absolute, err := filepath.Abs(path)
	if err != nil {
		return nil, ioError(err)
	}
	info, err := os.Stat(absolute)
	if err != nil {
		return nil, ioError(err)
	}
	if !info.Mode().IsRegular() {
		return nil, reject(CodeNotAFile, absolute+" is not a file")
	}
	if uint64(info.Size()) > limits.MaxPackBytes {
		return nil, tooLarge(limits.MaxPackBytes)
	}
	db, err := sql.Open("sqlite", uri(absolute))
	if err != nil {
		return nil, sqliteError(ctx, err)
	}
	// One connection holds every check; the pool never opens another.
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	db.SetConnMaxLifetime(0)
	db.SetConnMaxIdleTime(0)
	conn, err := db.Conn(ctx)
	if err != nil {
		_ = db.Close()
		return nil, sqliteError(ctx, err)
	}
	pack := &Pack{db: db, conn: conn, path: absolute, maxBytes: limits.MaxPackBytes}
	if err := pack.checkOpen(ctx, absolute, limits.MaxPackBytes); err != nil {
		_ = pack.Close()
		return nil, err
	}
	return pack, nil
}

func (p *Pack) checkOpen(ctx context.Context, absolute string, maximum uint64) error {
	// The driver applies these from the URI; a connection without them is
	// not used.
	var trusted, queryOnly int64
	if err := p.conn.QueryRowContext(ctx, "PRAGMA trusted_schema").Scan(&trusted); err != nil {
		return sqliteError(ctx, err)
	}
	if err := p.conn.QueryRowContext(ctx, "PRAGMA query_only").Scan(&queryOnly); err != nil {
		return sqliteError(ctx, err)
	}
	if trusted != 0 || queryOnly != 1 {
		return reject(CodeSQLite, "the SQLite connection did not take its defensive settings")
	}
	if err := checkIdentity(ctx, p.conn); err != nil {
		return err
	}
	// A pack is exactly its database: nothing, not even a sparse hole,
	// follows the last page, so hashing it never reads past the pages.
	pages, err := pragmaInteger(ctx, p.conn, "page_count")
	if err != nil {
		return err
	}
	info, err := os.Stat(absolute)
	if err != nil {
		return ioError(err)
	}
	length := uint64(info.Size())
	if pages < 0 || uint64(pages) > (1<<64-1)/PageSize || uint64(pages)*PageSize != length {
		return reject(CodeFileLength, fmt.Sprintf("the file is %d bytes, not its %d pages of %d bytes", length, pages, PageSize))
	}
	// The file SQLite opened may have replaced the one whose length was
	// checked, so its pages are checked against the limit too.
	if length > maximum {
		return tooLarge(maximum)
	}
	return nil
}

// Close closes the pack's connection.
func (p *Pack) Close() error {
	return errors.Join(p.conn.Close(), p.db.Close())
}

// Path returns the absolute path the pack was opened from.
func (p *Pack) Path() string {
	return p.path
}

func pragmaInteger(ctx context.Context, conn *sql.Conn, name string) (int64, error) {
	var value int64
	if err := conn.QueryRowContext(ctx, "PRAGMA "+name).Scan(&value); err != nil {
		return 0, sqliteError(ctx, err)
	}
	return value, nil
}

// checkIdentity checks the schema, application ID, user version, page size,
// free pages and meta of a pack before any other query runs.
func checkIdentity(ctx context.Context, conn *sql.Conn) error {
	// At most one row more than expected, each value bounded, is loaded.
	rows, err := conn.QueryContext(ctx, fmt.Sprintf(
		"SELECT %s, %s, %s FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name LIMIT %d",
		boundedText("type", "?1"), boundedText("name", "?1"), boundedText("sql", "?1"), len(schemaTables)+1,
	), maxSchemaBytes)
	if err != nil {
		return sqliteError(ctx, err)
	}
	var found [][3]sql.NullString
	for rows.Next() {
		var row [3]sql.NullString
		if err := rows.Scan(&row[0], &row[1], &row[2]); err != nil {
			_ = rows.Close()
			return sqliteError(ctx, err)
		}
		found = append(found, row)
	}
	if err := errors.Join(rows.Err(), rows.Close()); err != nil {
		return sqliteError(ctx, err)
	}
	matches := len(found) == len(schemaTables)
	for i := 0; matches && i < len(found); i++ {
		row, table := found[i], schemaTables[i]
		matches = row[0].Valid && row[0].String == "table" &&
			row[1].Valid && row[1].String == table[0] &&
			row[2].Valid && row[2].String == table[1]
	}
	if !matches {
		return reject(CodeSchema, "the pack schema does not match this format")
	}
	application, err := pragmaInteger(ctx, conn, "application_id")
	if err != nil {
		return err
	}
	version, err := pragmaInteger(ctx, conn, "user_version")
	if err != nil {
		return err
	}
	pageSize, err := pragmaInteger(ctx, conn, "page_size")
	if err != nil {
		return err
	}
	if application != ApplicationID || version != UserVersion || pageSize != PageSize {
		return reject(CodeIdentity, fmt.Sprintf("not a Tilewright pack of user_version %d with %d-byte pages", UserVersion, PageSize))
	}
	// Free pages hold no data, so a sparse file could make them holes that
	// cost nothing to store and much to hash; a pack has none, so the
	// integrity check reads every page as table data before it is hashed.
	free, err := pragmaInteger(ctx, conn, "freelist_count")
	if err != nil {
		return err
	}
	if free != 0 {
		return reject(CodeFreePages, fmt.Sprintf("the pack has %d free pages; a pack has none", free))
	}
	meta, err := readMeta(ctx, conn)
	if err != nil {
		return err
	}
	digest := meta["source_digest"]
	digestOK := len(digest) == 64
	for i := 0; digestOK && i < len(digest); i++ {
		digestOK = isDigit(digest[i]) || ('a' <= digest[i] && digest[i] <= 'f')
	}
	if meta["format"] != Format || meta["chunk_encoding"] != ChunkEncoding || !digestOK {
		return reject(CodeMeta, "the pack's metadata is not that of "+Format)
	}
	return nil
}

// readMeta reads meta after checking, without loading any value, that it
// holds exactly the format's keys with text values of bounded size.
func readMeta(ctx context.Context, conn *sql.Conn) (map[string]string, error) {
	quoted := make([]string, len(metaKeys))
	for i, key := range metaKeys {
		quoted[i] = "'" + key + "'"
	}
	var total, known, oversized int64
	err := conn.QueryRowContext(ctx, fmt.Sprintf(
		"SELECT count(*), count(*) FILTER (WHERE key IN (%s)), "+
			"count(*) FILTER (WHERE typeof(value) != 'text' OR octet_length(value) > CASE key WHEN 'profile' THEN ?1 ELSE ?2 END) "+
			"FROM meta", strings.Join(quoted, ", ")),
		maxDocumentBytes, maxMetaBytes,
	).Scan(&total, &known, &oversized)
	if err != nil {
		return nil, sqliteError(ctx, err)
	}
	if total != int64(len(metaKeys)) || known != int64(len(metaKeys)) || oversized != 0 {
		return nil, reject(CodeMeta, "the pack's metadata is not that of "+Format)
	}
	rows, err := conn.QueryContext(ctx, "SELECT key, value FROM meta ORDER BY key")
	if err != nil {
		return nil, sqliteError(ctx, err)
	}
	meta := make(map[string]string, len(metaKeys))
	for rows.Next() {
		var key, value string
		if err := rows.Scan(&key, &value); err != nil {
			_ = rows.Close()
			return nil, sqliteError(ctx, err)
		}
		if !validUTF8(value) {
			_ = rows.Close()
			return nil, reject(CodeMeta, "a stored value is not UTF-8 text")
		}
		meta[key] = value
	}
	if err := errors.Join(rows.Err(), rows.Close()); err != nil {
		return nil, sqliteError(ctx, err)
	}
	if len(meta) != len(metaKeys) {
		return nil, reject(CodeMeta, "the pack's metadata is not that of "+Format)
	}
	return meta, nil
}

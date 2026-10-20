// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack

import (
	"context"
	"database/sql"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

// validFixture copies the shared valid pack into a new directory, under
// name when it is not empty, and returns the copy's path.
func validFixture(t *testing.T, directory, name string) string {
	t.Helper()
	source := filepath.Join(fixtures, loadManifest(t).Valid[0].File)
	data, err := os.ReadFile(source)
	if err != nil {
		t.Fatal(err)
	}
	if directory == "" {
		directory = t.TempDir()
	}
	if name == "" {
		name = filepath.Base(source)
	}
	path := filepath.Join(directory, name)
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

// tampered copies the valid pack and runs statements on the copy through a
// writable connection.
func tampered(t *testing.T, statements string) string {
	t.Helper()
	path := validFixture(t, "", "tampered.twpack")
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec(statements); err != nil {
		t.Fatal(err)
	}
	return path
}

func openPack(t *testing.T, path string, limits Limits) *Pack {
	t.Helper()
	pack, err := Open(context.Background(), path, limits)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = pack.Close() })
	return pack
}

func expectCode(t *testing.T, what string, err error, code Code) {
	t.Helper()
	if CodeOf(err) != code {
		t.Fatalf("%s: %v (%q), expected %q", what, err, CodeOf(err), code)
	}
}

func TestOpenRejectsMissingFilesAndDirectories(t *testing.T) {
	ctx := context.Background()
	_, err := Open(ctx, filepath.Join(t.TempDir(), "missing.twpack"), Limits{})
	expectCode(t, "a missing file", err, CodeIO)
	if !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("%v does not wrap the I/O error", err)
	}
	_, err = Open(ctx, t.TempDir(), Limits{})
	expectCode(t, "a directory", err, CodeNotAFile)
}

func TestConnectionsAreReadOnlyDefensiveAndDistrustSchemas(t *testing.T) {
	ctx := context.Background()
	pack := openPack(t, validFixture(t, "", ""), Limits{})
	for pragma, expected := range map[string]int64{"trusted_schema": 0, "query_only": 1} {
		if value, err := pragmaInteger(ctx, pack.conn, pragma); err != nil || value != expected {
			t.Errorf("PRAGMA %s = %d (%v), expected %d", pragma, value, err, expected)
		}
	}
	// Defensive mode turns writable_schema into a no-op.
	if _, err := pack.conn.ExecContext(ctx, "PRAGMA writable_schema = ON"); err != nil {
		t.Fatal(err)
	}
	if value, err := pragmaInteger(ctx, pack.conn, "writable_schema"); err != nil || value != 0 {
		t.Errorf("writable_schema = %d (%v) on a defensive connection", value, err)
	}
	// Turning query_only off still leaves a read-only connection.
	if _, err := pack.conn.ExecContext(ctx, "PRAGMA query_only = OFF"); err != nil {
		t.Fatal(err)
	}
	if _, err := pack.conn.ExecContext(ctx, "CREATE TABLE extra (x)"); err == nil {
		t.Fatal("a pack connection could write")
	}
}

func TestPacksPastTheSizeLimitAreNotOpenedOrHashed(t *testing.T) {
	ctx := context.Background()
	directory := t.TempDir()
	path := validFixture(t, directory, "")
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	length := uint64(info.Size())
	exact, short := Limits{MaxPackBytes: length}, Limits{MaxPackBytes: length - 1}
	pack := openPack(t, path, exact)
	if _, err := pack.Verify(ctx, exact); err != nil {
		t.Fatal(err)
	}
	digest, err := pack.FileDigest(ctx)
	if err != nil || hex.EncodeToString(digest[:]) != strings.TrimSuffix(filepath.Base(path), ".twpack") {
		t.Fatalf("digest %x (%v)", digest, err)
	}
	_, err = pack.Verify(ctx, short)
	expectCode(t, "verifying with a lower limit", err, CodeTooLarge)
	_, err = Open(ctx, path, short)
	expectCode(t, "opening past the limit", err, CodeTooLarge)

	// The length is checked before SQLite opens the file: a sparse file past
	// the default limit is rejected without being read.
	sparse := filepath.Join(directory, "sparse.twpack")
	if err := os.WriteFile(sparse, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Truncate(sparse, DefaultMaxPackBytes+1); err != nil {
		t.Fatal(err)
	}
	_, err = Open(ctx, sparse, Limits{})
	expectCode(t, "a sparse file past the default limit", err, CodeTooLarge)

	// Hashing and verifying stop at the limit the pack was opened with,
	// whatever limits verification is given, even if the file has grown.
	if err := os.Truncate(path, int64(length)+1); err != nil {
		t.Fatal(err)
	}
	_, err = pack.FileDigest(ctx)
	expectCode(t, "hashing a grown file", err, CodeTooLarge)
	for _, limits := range []Limits{{}, {MaxPackBytes: length + 1}} {
		_, err = pack.Verify(ctx, limits)
		expectCode(t, "verifying a grown file", err, CodeTooLarge)
	}
}

func TestTheNameDigestBindsToThePathAndIsLowercase(t *testing.T) {
	ctx := context.Background()
	directory := t.TempDir()
	path := validFixture(t, directory, "")
	stem := strings.TrimSuffix(filepath.Base(path), ".twpack")
	upper := validFixture(t, t.TempDir(), strings.ToUpper(stem)+".twpack")
	_, err := openPack(t, upper, Limits{}).Verify(ctx, Limits{})
	expectCode(t, "an uppercase digest name", err, CodeDigest)

	// Replacing the file at the path after opening changes what is hashed.
	pack := openPack(t, path, Limits{})
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	data[len(data)-1] ^= 1
	replacement := filepath.Join(directory, "replacement")
	if err := os.WriteFile(replacement, data, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(replacement, path); err != nil {
		t.Fatal(err)
	}
	_, err = pack.Verify(ctx, Limits{})
	expectCode(t, "a replaced file", err, CodeDigest)
}

func TestPathsAreEncodedForSQLite(t *testing.T) {
	ctx := context.Background()
	for _, name := range []string{"with space", "a?b", "a#b", "100%", "a&_pragma=x", "é", "\xff", "file:x"} {
		directory := filepath.Join(t.TempDir(), name)
		if err := os.Mkdir(directory, 0o700); err != nil {
			t.Logf("the file system refuses %q: %v", name, err)
			continue
		}
		pack := openPack(t, validFixture(t, directory, ""), Limits{})
		if _, err := pack.Verify(ctx, Limits{}); err != nil {
			t.Errorf("%q: %v", name, err)
		}
	}
}

func TestOversizedChunkDataIsRejectedBeforeLoading(t *testing.T) {
	ctx := context.Background()
	for _, statement := range []string{
		"UPDATE chunks SET data = zeroblob(16842753) WHERE plane = 'vault'; VACUUM",
		"UPDATE chunks SET data = x'' WHERE plane = 'vault'; VACUUM",
	} {
		pack := openPack(t, tampered(t, statement), Limits{})
		code := CodeChunkData
		if strings.Contains(statement, "x''") {
			code = CodeChunkRecord
		}
		_, _, err := pack.Chunk(ctx, "vault", 0, 0, 0)
		expectCode(t, statement, err, code)
		_, err = pack.Verify(ctx, Limits{})
		expectCode(t, statement, err, code)
	}
}

func TestCanceledContextsAreNotPackErrors(t *testing.T) {
	path := validFixture(t, "", "")
	pack := openPack(t, path, Limits{})
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	for what, err := range map[string]error{
		"open":   func() error { _, err := Open(ctx, path, Limits{}); return err }(),
		"verify": func() error { _, err := pack.Verify(ctx, Limits{}); return err }(),
		"chunk":  func() error { _, _, err := pack.Chunk(ctx, "vault", 0, 0, 0); return err }(),
	} {
		if !errors.Is(err, context.Canceled) || CodeOf(err) != "" {
			t.Errorf("%s: %v (%q)", what, err, CodeOf(err))
		}
	}
}

func TestIteratorsReleaseTheirRowsWhenStoppedEarly(t *testing.T) {
	ctx := context.Background()
	pack := openPack(t, validFixture(t, "", ""), Limits{})
	for range 3 {
		for definition, err := range pack.Definitions(ctx) {
			if err != nil || definition.ID == "" {
				t.Fatal(definition, err)
			}
			// Reads inside an iteration share the connection.
			if _, ok, err := pack.Plane(ctx, "vault"); err != nil || !ok {
				t.Fatal(ok, err)
			}
			break
		}
	}
	if _, err := pack.Verify(ctx, Limits{}); err != nil {
		t.Fatal(err)
	}
	if err := pack.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestPacksServeConcurrentReads(t *testing.T) {
	ctx := context.Background()
	pack := openPack(t, validFixture(t, "", ""), Limits{})
	var group sync.WaitGroup
	errs := make(chan error, 16)
	for range 16 {
		group.Go(func() {
			for range 20 {
				chunk, ok, err := pack.Chunk(ctx, "overworld", 0, 0, 0)
				if err != nil || !ok || len(chunk.Objects) == 0 {
					errs <- errors.Join(err, errors.New("chunk read failed"))
					return
				}
			}
		})
	}
	group.Wait()
	close(errs)
	for err := range errs {
		t.Error(err)
	}
}

func TestZeroLimitsTakeTheDefaults(t *testing.T) {
	limits := Limits{}.withDefaults()
	if limits.MaxPackBytes != 8<<30 || limits.MaxDecodedBytes != 8<<30 {
		t.Fatalf("%+v", limits)
	}
}

func TestVerificationCapsZonesPerPlane(t *testing.T) {
	// 250,001 root zones in one plane: more than its one source document
	// holds, so the plane's parent chains are not followed.
	path := tampered(t, `DELETE FROM zones WHERE plane = 'vault';
		WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 250001)
		INSERT INTO zones SELECT 'vault', 'z' || i, NULL, '{"id":"z' || i || '"}' FROM n;`)
	_, err := openPack(t, path, Limits{}).Verify(context.Background(), Limits{})
	expectCode(t, "a plane with too many zones", err, CodeZone)
	if !strings.Contains(err.Error(), "more zones") {
		t.Fatal(err)
	}
}

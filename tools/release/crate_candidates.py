"""Carry exact, independently versioned crate candidates in a source release.

Only byte validation runs here: no Cargo, registry access or credentials.
"""
import argparse
import gzip
import importlib.util
import io
from pathlib import Path
import tarfile

SPEC = importlib.util.spec_from_file_location(
    "world_crate_prepare", Path(__file__).resolve().parents[1] / "crates/prepare.py")
CANDIDATES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CANDIDATES)
MAX_FILE_BYTES = 128 * 1024 * 1024
MAX_BUNDLE_BYTES = 256 * 1024 * 1024


def name(version):
    return f"tilewright-world-{version}-crate-candidates.tar.gz"


def check(files, revision):
    manifest, _ = CANDIDATES.verify_files(files)
    CANDIDATES.require(manifest["revision"] == revision,
                       "crate candidates are from a different source revision")


def bundle(directory, revision):
    files = {}
    CANDIDATES.require(directory.is_dir() and not directory.is_symlink(),
                       "candidate directory must be a real directory")
    version, _ = CANDIDATES.policy()
    expected = {"candidate.json", *(f"{name}-{version}.crate" for name in CANDIDATES.PUBLIC)}
    CANDIDATES.require({path.name for path in directory.iterdir()} == expected,
                       "candidate directory inventory differs")
    total = 0
    for path in directory.iterdir():
        CANDIDATES.require(path.is_file() and not path.is_symlink(),
                           "candidate asset must be a regular file")
        size = path.stat().st_size
        total += size
        CANDIDATES.require(size <= MAX_FILE_BYTES and total <= MAX_BUNDLE_BYTES,
                           "candidate asset size limit exceeded")
        files[path.name] = path.read_bytes()
    check(files, revision)
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for filename, data in sorted(files.items()):
            item = tarfile.TarInfo(filename)
            item.size = len(data)
            item.mode = 0o644
            archive.addfile(item, io.BytesIO(data))
    result = gzip.compress(output.getvalue(), mtime=0)
    verify(result, revision)
    return result


def verify(data, revision):
    files = {}
    total = 0
    version, _ = CANDIDATES.policy()
    expected = {"candidate.json", *(f"{name}-{version}.crate" for name in CANDIDATES.PUBLIC)}
    CANDIDATES.require(len(data) <= MAX_BUNDLE_BYTES, "candidate bundle size limit exceeded")
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for item in archive:
            CANDIDATES.require(item.isfile() and item.name in expected and item.name not in files,
                               "unsafe or duplicate candidate bundle entry")
            total += item.size
            CANDIDATES.require(item.size <= MAX_FILE_BYTES and total <= MAX_BUNDLE_BYTES,
                               "candidate bundle size limit exceeded")
            files[item.name] = archive.extractfile(item).read()
    check(files, revision)


def verify_path(path, revision):
    CANDIDATES.require(path.is_file() and not path.is_symlink() and
                       path.stat().st_size <= MAX_BUNDLE_BYTES,
                       "candidate bundle must be a bounded regular file")
    verify(path.read_bytes(), revision)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--revision", required=True)
    args = parser.parse_args()
    try:
        verify_path(args.bundle, args.revision)
    except (ValueError, OSError, KeyError, TypeError, tarfile.TarError) as error:
        parser.exit(1, f"crate candidates: {error}\n")
    print("Verified candidate bundle; no registry ownership or publication is implied")

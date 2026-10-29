#!/usr/bin/env python3
"""Prepare and inspect local crate candidates; no registry or release writes."""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PUBLIC = ("tilewright-jsonc", "tilewright-source", "tilewright-pack",
          "tilewright-diff", "tilewright-edit")
PRIVATE = ("tilewright-cli", "tilewright-pack-wasm")
REPOSITORY = "https://github.com/seraphinastudios/tilewright-world"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load_toml(path):
    return tomllib.loads(path.read_text())


def policy(root=ROOT):
    config = json.loads((root / "tools/crates/candidates.json").read_text())
    require(config == {"schema": 1, "status": "candidate-only", "registry": "crates-io",
                       "crates": list(PUBLIC), "private": list(PRIVATE)}, "candidate allowlist changed")
    workspace = load_toml(root / "Cargo.toml")["workspace"]
    require(workspace["package"].get("license") == "MIT" and
            workspace["package"].get("license-file") == "LICENSE" and (root / "LICENSE").is_file(),
            "workspace must provide the root MIT license file")
    require(set(workspace["members"]) == {f"crates/{name}" for name in PUBLIC + PRIVATE},
            "workspace membership differs from candidate policy")
    version = workspace["package"]["version"]
    require(re.fullmatch(r"0|[1-9][0-9]*", version.split(".")[0]) and
            re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version),
            "candidate version must be a release version")
    for name in PUBLIC + PRIVATE:
        package = load_toml(root / f"crates/{name}/Cargo.toml")["package"]
        require(package["name"] == name and package["version"] == {"workspace": True},
                "candidate package identity differs")
        require(package.get("publish") == (["crates-io"] if name in PUBLIC else False),
                "candidate/private publication metadata differs")
        if name in PUBLIC:
            require(package.get("license") == {"workspace": True} and
                    package.get("license-file") == {"workspace": True},
                    "public candidate must inherit the root MIT license file")
    for name in PUBLIC:
        dependency = workspace["dependencies"][name]
        require(dependency["version"] == f"={version}" and dependency["path"] == f"crates/{name}",
                "internal dependency must have the exact candidate version")
    toolchain = load_toml(root / "rust-toolchain.toml")["toolchain"]["channel"]
    return version, toolchain


def environment():
    # Fail rather than silently carry registry or GitHub credentials into Cargo.
    forbidden = ("GH_TOKEN", "GITHUB_TOKEN", "GH_READ_TOKEN", "GH_INTENT_TOKEN",
                 "ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN")
    require(not any(value and (key in forbidden or key.startswith("CARGO_REGISTR"))
                    for key, value in os.environ.items()), "use a credential-free environment")
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    require(not any((cargo_home / name).exists() for name in ("credentials", "credentials.toml")),
            "use a Cargo home without credential files")
    result = os.environ.copy()
    result["CARGO_NET_OFFLINE"] = "true"
    return result


def run(arguments, *, cwd=ROOT, env=None):
    return subprocess.check_output(arguments, cwd=cwd, env=env, text=True, timeout=1800).strip()


def reserve_output(path, root=ROOT):
    path = Path(os.path.abspath(path))
    require(not path.is_relative_to(root.resolve()), "output must be outside the source checkout")
    require(path.parent.is_dir(), "output parent must already exist")
    require(not any(part.is_symlink() for part in (path, *path.parents)), "output path contains symlink")
    # Exclusive mkdir also rejects empty directories, dangling symlinks and collisions.
    path.mkdir()
    return path


def digest(data):
    return hashlib.sha256(data).hexdigest()


def archive(data, name, version, revision):
    files = {}
    prefix = f"{name}-{version}"
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as stream:
        for item in stream:
            path = PurePosixPath(item.name)
            require(item.isfile() and len(path.parts) > 1 and path.parts[0] == prefix and
                    not path.is_absolute() and ".." not in path.parts and
                    path.as_posix() == item.name and item.name not in files,
                    "unsafe or duplicate archive entry")
            require(item.size <= 32 * 1024 * 1024 and len(files) < 10000,
                    "archive entry limit exceeded")
            require(sum(map(len, files.values())) + item.size <= 128 * 1024 * 1024,
                    "archive size limit exceeded")
            files[item.name] = stream.extractfile(item).read()
    relative = {path[len(prefix) + 1:]: value for path, value in files.items()}
    require({"Cargo.toml", "Cargo.toml.orig", ".cargo_vcs_info.json", "LICENSE", "README.md"} <= relative.keys(),
            "candidate archive lacks source metadata or notices")
    package = tomllib.loads(relative["Cargo.toml"].decode())["package"]
    require(package["name"] == name and package["version"] == version and
            package["publish"] == ["crates-io"] and package["license"] == "MIT" and
            package.get("license-file") == "LICENSE" and
            package["repository"] == REPOSITORY, "archive package identity differs")
    require(relative["LICENSE"] == (ROOT / "LICENSE").read_bytes(),
            "archive must contain the exact root MIT license notice")
    require(json.loads(relative[".cargo_vcs_info.json"]) ==
            {"git": {"sha1": revision}, "path_in_vcs": f"crates/{name}"},
            "archive does not identify the clean candidate revision")
    manifest = tomllib.loads(relative["Cargo.toml"].decode())
    tables = [manifest, *manifest.get("target", {}).values()]
    for table in tables:
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            for dependency, specification in table.get(key, {}).items():
                require(not isinstance(specification, dict) or
                        not {"path", "git", "registry", "registry-index"} & specification.keys(),
                        "archive contains a non-crates.io dependency")
                if dependency in PUBLIC:
                    require(specification["version"] == f"={version}", "archive graph version differs")
    return relative


def package_row(name, version, data, revision):
    files = archive(data, name, version, revision)
    return {"name": name, "version": version, "path": f"crates/{name}",
            "asset": f"{name}-{version}.crate", "sha256": digest(data),
            "inventory": [{"path": path, "sha256": digest(value), "size": len(value)}
                          for path, value in sorted(files.items())]}


def prepare(output):
    env = environment()
    version, toolchain = policy()
    require(not run(["git", "status", "--porcelain", "--untracked-files=all"]),
            "candidate packaging requires a clean committed checkout")
    revision = run(["git", "rev-parse", "HEAD"])
    output = reserve_output(output)
    with tempfile.TemporaryDirectory(prefix="tilewright-world-candidates-", dir=output) as scratch:
        rounds = []
        for number in (1, 2):
            target = Path(scratch) / str(number)
            command = ["cargo", f"+{toolchain}", "package", "--locked", "--offline", "--no-verify",
                       "--registry", "crates-io", "--target-dir", str(target)]
            for name in PUBLIC:
                command += ["--package", name]
            subprocess.run(command, cwd=ROOT, env=env, check=True, timeout=1800)
            rounds.append({name: (target / "package" / f"{name}-{version}.crate").read_bytes()
                           for name in PUBLIC})
        require(rounds[0] == rounds[1], "crate archives differ between packaging rounds")
        rows = [package_row(name, version, rounds[0][name], revision) for name in PUBLIC]
        for row in rows:
            (output / row["asset"]).write_bytes(rounds[0][row["name"]])
    require(run(["git", "rev-parse", "HEAD"]) == revision and
            not run(["git", "status", "--porcelain", "--untracked-files=all"]),
            "source changed during packaging")
    manifest = {"schema": 1, "status": "candidate-only", "repository": REPOSITORY,
                "revision": revision, "toolchain": toolchain, "registry": "crates-io",
                "reproducible_packages": True, "packages": rows}
    (output / "candidate.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verify(output, build=False)


def verify(output, build=False):
    env = environment()
    output = Path(output)
    require(output.is_dir() and not output.is_symlink(), "candidate directory must be a real directory")
    require(not (output / "candidate.json").is_symlink(), "candidate manifest must not be a symlink")
    files = {}
    for path in output.iterdir():
        require(path.is_file() and not path.is_symlink(), "candidate asset must be a regular file")
        files[path.name] = path.read_bytes()
    manifest, archives = verify_files(files)
    version, toolchain = policy()
    if build:
        build_graph(archives, version, toolchain, env)
    print(f"Verified {len(archives)} local candidate archives" + (" and compiled isolated graph" if build else ""))


def verify_files(files):
    """Check candidate bytes without executing Cargo or accessing credentials.

    Release verification uses this pure boundary in privileged jobs; callers
    must separately authenticate the expected source revision and origin.
    """
    manifest = json.loads(files["candidate.json"])
    version, toolchain = policy()
    require(set(manifest) == {"schema", "status", "repository", "revision", "toolchain", "registry",
                              "reproducible_packages", "packages"} and
            manifest["schema"] == 1 and manifest["status"] == "candidate-only" and
            manifest["repository"] == REPOSITORY and manifest["registry"] == "crates-io" and
            manifest["toolchain"] == toolchain and manifest["reproducible_packages"] is True and
            re.fullmatch(r"[0-9a-f]{40}", manifest["revision"]), "invalid candidate manifest")
    require([row["name"] for row in manifest["packages"]] == list(PUBLIC),
            "candidate manifest package allowlist differs")
    expected = {"candidate.json", *(f"{name}-{version}.crate" for name in PUBLIC)}
    require(set(files) == expected, "candidate directory inventory differs")
    archives = {}
    for row in manifest["packages"]:
        asset = f"{row['name']}-{version}.crate"
        data = files[asset]
        require(row == package_row(row["name"], version, data, manifest["revision"]),
                "candidate manifest digest, identity or inventory differs")
        archives[row["name"]] = archive(data, row["name"], version, manifest["revision"])
    return manifest, archives


def build_graph(archives, version, toolchain, env):
    with tempfile.TemporaryDirectory(prefix="tilewright-world-verify-") as scratch:
        root = Path(scratch)
        for name, files in archives.items():
            for path, data in files.items():
                destination = root / f"{name}-{version}" / path
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(data)
        patches = []
        for name in PUBLIC:
            patches += ["--config", f'patch.crates-io.{name}.path={json.dumps(str(root / f"{name}-{version}"))}']
        build_env = dict(env, CARGO_TARGET_DIR=str(root / "target"))
        # Tests need repository fixtures at runtime. Compile them here; the source
        # validation suite runs them with fixtures in the owning checkout.
        for name in PUBLIC:
            subprocess.run(["cargo", f"+{toolchain}", "test", "--offline", "--all-features", "--no-run",
                            "--manifest-path", str(root / f"{name}-{version}/Cargo.toml"), *patches],
                           cwd=root, env=build_env, check=True, timeout=1800)
        consumer = root / "consumer"
        (consumer / "src").mkdir(parents=True)
        (consumer / "Cargo.toml").write_text(
            '[package]\nname="candidate-consumer"\nversion="0.0.0"\nedition="2024"\n[dependencies]\n' +
            "".join(f'{name}="={version}"\n' for name in PUBLIC))
        (consumer / "src/lib.rs").write_text("".join(f"pub use {name.replace('-', '_')};\n" for name in PUBLIC))
        subprocess.run(["cargo", f"+{toolchain}", "check", "--offline", "--manifest-path",
                        str(consumer / "Cargo.toml"), *patches], cwd=root, env=build_env,
                       check=True, timeout=1800)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("prepare").add_argument("output", type=Path)
    verifier = commands.add_parser("verify")
    verifier.add_argument("output", type=Path)
    verifier.add_argument("--build", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(args.output)
        else:
            verify(args.output, args.build)
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError, tarfile.TarError) as error:
        parser.exit(1, f"candidate preparation: {error}\n")


if __name__ == "__main__":
    main()

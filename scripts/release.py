#!/usr/bin/env python3
"""Build a tagged source tree and assemble versioned, offline-verifiable assets."""

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parent.parent
REPOSITORY = "https://github.com/ramonmello/tmux-agent-usage"
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
)


def run(*args, **kwargs):
    return subprocess.check_output(args, cwd=ROOT, text=True, **kwargs).strip()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def source(tag):
    # A release must come from a clean checkout of this repository's root.
    require(Path(run("git", "rev-parse", "--show-toplevel")).resolve() == ROOT,
            "release source must be the repository's Git root")
    require(not run("git", "status", "--porcelain", "--untracked-files=all"),
            "release source must be clean, including untracked files")
    versions = [line.split('"')[1] for line in (ROOT / "Cargo.toml").read_text().splitlines()
                if line.startswith('version = "')]
    require(len(versions) == 1 and tag == "v" + versions[0],
            "tag must be vVERSION from Cargo.toml")
    commit = run("git", "rev-parse", "HEAD")
    require(run("git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}") == commit,
            "tag must resolve to HEAD")
    return {"repository": REPOSITORY, "tag": tag, "commit": commit,
            "version": versions[0], "source_date_epoch": int(run("git", "show", "-s", "--format=%ct", "HEAD")),
            "lockfile_sha256": digest((ROOT / "Cargo.lock").read_bytes())}


def check_binary(data, target):
    """Check the executable ABI without executing it or trusting file(1) output."""
    require(len(data) >= 64, "truncated executable")
    if target.endswith("apple-darwin"):
        magic, cpu, _, kind, count, size = struct.unpack_from("<6I", data)
        require(magic == 0xFEEDFACF and kind == 2 and cpu ==
                (0x100000C if target.startswith("aarch64") else 0x1000007),
                "expected a thin Mach-O executable for target")
        end = 32 + size
        require(end <= len(data), "truncated Mach-O commands")
        offset, minimum = 32, None
        for _ in range(count):
            require(offset + 8 <= end, "truncated Mach-O command")
            command, length = struct.unpack_from("<2I", data, offset)
            require(length >= 8 and offset + length <= end, "invalid Mach-O command size")
            if command in (0x24, 0x32):
                require(length >= 16, "truncated deployment target")
                minimum = struct.unpack_from("<I", data, offset + (12 if command == 0x32 else 8))[0]
            # LC_LOAD_DYLIB, LC_LOAD_WEAK_DYLIB, LC_REEXPORT_DYLIB, LC_LOAD_UPWARD_DYLIB.
            if command in (0xC, 0x80000018, 0x8000001F, 0x80000023):
                require(length >= 24, "truncated dylib command")
                start = struct.unpack_from("<I", data, offset + 8)[0]
                require(24 <= start < length, "invalid dylib path")
                name = data[offset + start:offset + length].split(b"\0", 1)[0]
                require(name.startswith((b"/usr/lib/", b"/System/Library/")),
                        "non-system macOS dynamic library")
            offset += length
        require(offset == end and minimum == 11 << 16,
                "macOS deployment target must be exactly 11.0")
    else:
        require(data[:7] == b"\x7fELF\x02\x01\x01", "expected ELF64 little-endian executable")
        kind, machine = struct.unpack_from("<2H", data, 16)
        require(kind in (2, 3) and machine == (183 if target.startswith("aarch64") else 62),
                "ELF executable architecture does not match target")
        phoff = struct.unpack_from("<Q", data, 32)[0]
        entry_size, count = struct.unpack_from("<2H", data, 54)
        require(count > 0 and entry_size == 56 and phoff + count * entry_size <= len(data),
                "invalid ELF program headers")
        for index in range(count):
            offset = phoff + index * entry_size
            segment, _, start, _, _, size = struct.unpack_from("<II4Q", data, offset)
            require(segment != 3, "Linux release must have no dynamic interpreter (PT_INTERP)")
            if segment == 2:
                require(start + size <= len(data) and size % 16 == 0, "invalid ELF dynamic segment")
                for position in range(start, start + size, 16):
                    dynamic_tag = struct.unpack_from("<Q", data, position)[0]
                    require(dynamic_tag != 1, "Linux release must have no shared libraries (DT_NEEDED)")
                    if dynamic_tag == 0:
                        break


def output_stage(destination):
    destination = destination.resolve()
    require(not destination.exists(), "output directory already exists")
    require(not destination.is_relative_to(ROOT), "output directory must be outside source")
    return tempfile.TemporaryDirectory(prefix=".agent-usage-release-", dir=destination.parent)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def archive_binary(data, epoch):
    # Stable owner, mode, member order, timestamp and gzip header; compiler/SDK
    # output can still differ. This is not a bit-for-bit reproducible-build claim.
    buffer = io.BytesIO()
    with gzip.GzipFile(fileobj=buffer, mode="wb", filename="", mtime=epoch) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            member = tarfile.TarInfo("tmux-agent-usage")
            member.mode, member.size, member.mtime = 0o755, len(data), epoch
            archive.addfile(member, io.BytesIO(data))
    return buffer.getvalue()


def build(tag, target, destination):
    origin = source(tag)
    environment = os.environ.copy()
    environment.pop("CARGO_ENCODED_RUSTFLAGS", None)
    environment["RUSTFLAGS"] = "-C target-feature=+crt-static" if "linux" in target else ""
    environment["MACOSX_DEPLOYMENT_TARGET"] = "11.0"
    environment["SOURCE_DATE_EPOCH"] = str(origin["source_date_epoch"])
    with output_stage(destination) as directory:
        staging = Path(directory)
        environment["CARGO_TARGET_DIR"] = str(staging / "build")
        command = ["cargo", "build", "--locked", "--release", "--target", target, "--bin", "tmux-agent-usage"]
        subprocess.run(command, cwd=ROOT, env=environment, check=True)
        binary = (staging / "build" / target / "release/tmux-agent-usage").read_bytes()
        check_binary(binary, target)
        require(source(tag) == origin, "source changed during build")
        assets = staging / "assets"
        assets.mkdir()
        name = f"tmux-agent-usage-{tag}-{target}.tar.gz"
        packed = archive_binary(binary, origin["source_date_epoch"])
        (assets / name).write_bytes(packed)
        (assets / (name + ".sha256")).write_text(f"{digest(packed)}  {name}\n")
        write_json(assets / (name + ".json"), {
            **origin, "target": target, "artifact": name, "sha256": digest(packed),
            "binary_sha256": digest(binary), "rustc": run("rustc", "-vV"),
            "cargo": run("cargo", "-V"), "build_command": command,
            "rustflags": environment["RUSTFLAGS"], "macos_deployment_target": "11.0" if "darwin" in target else None,
        })
        assets.rename(destination)


def assemble(tag, inputs, destination):
    origin = source(tag)
    records, payloads = [], {}
    for target in TARGETS:
        name = f"tmux-agent-usage-{tag}-{target}.tar.gz"
        matches = list(inputs.rglob(name))
        require(len(matches) == 1, f"expected exactly one artifact for {target}")
        archive = matches[0]
        require(not archive.is_symlink(), "artifact cannot be a symlink")
        packed = archive.read_bytes()
        checksum = f"{digest(packed)}  {name}\n".encode()
        require(archive.with_name(name + ".sha256").read_bytes() == checksum, "checksum mismatch")
        record_bytes = archive.with_name(name + ".json").read_bytes()
        record = json.loads(record_bytes)
        require(all(record.get(key) == value for key, value in origin.items()), "artifact source mismatch")
        require(record["target"] == target and record["artifact"] == name and record["sha256"] == digest(packed),
                "artifact provenance mismatch")
        with tarfile.open(fileobj=io.BytesIO(packed), mode="r:gz") as contents:
            members = contents.getmembers()
            require(len(members) == 1 and members[0].name == "tmux-agent-usage" and members[0].isreg(),
                    "archive must contain only one regular tmux-agent-usage file")
            require(members[0].mode == 0o755, "archive executable mode must be 0755")
            binary = contents.extractfile(members[0]).read()
        check_binary(binary, target)
        require(digest(binary) == record["binary_sha256"], "binary provenance mismatch")
        payloads.update({name: packed, name + ".sha256": checksum, name + ".json": record_bytes})
        records.append(record)
    with output_stage(destination) as directory:
        staging = Path(directory)
        for name, data in payloads.items():
            (staging / name).write_bytes(data)
        write_json(staging / f"provenance-{tag}.json", {**origin, "artifacts": records})
        # Also authenticate the per-target provenance; sidecars cover archives.
        sums = [f"{digest(path.read_bytes())}  {path.name}\n" for path in sorted(staging.iterdir())]
        (staging / "SHA256SUMS").write_text("".join(sums))
        staging.rename(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    origin = commands.add_parser("source", help="verify a clean source checkout and tag")
    origin.add_argument("tag")
    builder = commands.add_parser("build", help="build and package one target")
    builder.add_argument("tag")
    builder.add_argument("target", choices=TARGETS)
    builder.add_argument("output", type=Path)
    merger = commands.add_parser("assemble", help="verify and combine all four targets")
    merger.add_argument("tag")
    merger.add_argument("inputs", type=Path)
    merger.add_argument("output", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "source":
            print(source(args.tag)["commit"])
        elif args.command == "build":
            build(args.tag, args.target, args.output.resolve())
        else:
            assemble(args.tag, args.inputs.resolve(), args.output.resolve())
    except (ValueError, OSError, subprocess.CalledProcessError, KeyError, struct.error, tarfile.TarError) as error:
        parser.exit(1, f"release.py: {error}\n")


if __name__ == "__main__":
    main()

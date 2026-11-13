"""Release producer tests, invoked by cargo test; all source and tools are local."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("release", ROOT / "scripts/release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


def executable(target):
    """Synthetic headers for producer/ABI tests; never a runtime validation."""
    data = bytearray(128)
    if "darwin" in target:
        struct.pack_into("<8I", data, 0, 0xFEEDFACF,
                         0x100000C if target.startswith("aarch64") else 0x1000007,
                         0, 2, 1, 24, 0, 0)
        struct.pack_into("<6I", data, 32, 0x32, 24, 1, 11 << 16, 0, 0)
    else:
        data[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<2H", data, 16, 2, 183 if target.startswith("aarch64") else 62)
        struct.pack_into("<Q", data, 32, 64)
        struct.pack_into("<2H", data, 54, 56, 1)
        struct.pack_into("<II", data, 64, 1, 5)  # PT_LOAD, RX
    return bytes(data)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="agent-usage-release-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source with spaces"
        self.source.mkdir()
        (self.source / "scripts").mkdir()
        for script in ["release.py", "ci.sh", "plugin-helpers.sh", "install-release.sh"]:
            shutil.copy2(ROOT / "scripts" / script, self.source / "scripts" / script)
        (self.source / "Cargo.toml").write_text('[package]\nname = "tmux-agent-usage"\nversion = "0.1.0"\n')
        (self.source / "Cargo.lock").write_text("synthetic locked dependency inventory\n")
        self.tools = self.root / "fake tools"
        self.tools.mkdir()
        self.environment = dict(os.environ, PATH=str(self.tools) + os.pathsep + os.environ["PATH"],
                                GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                                GIT_AUTHOR_DATE="2026-11-12T00:00:00Z", GIT_COMMITTER_DATE="2026-11-12T00:00:00Z",
                                RELEASE_FIXTURE_LOG=str(self.root / "calls.jsonl"))
        for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_CONFIG_COUNT"]:
            self.environment.pop(key, None)
        self.git("init", "-q")
        self.commit()
        self.git("tag", "-a", "v0.1.0", "-m", "Synthetic release fixture")
        self.tool("cargo", '''import json, os, pathlib, shutil, sys
with open(os.environ["RELEASE_FIXTURE_LOG"], "a") as log:
    log.write(json.dumps({"args": sys.argv[1:], "rustflags": os.getenv("RUSTFLAGS"),
                         "deployment": os.getenv("MACOSX_DEPLOYMENT_TARGET")}) + "\\n")
if sys.argv[1:] == ["-V"]:
    print("cargo 1.89.0 (synthetic build driver)")
else:
    assert sys.argv[1:] == ["build", "--locked", "--release", "--target", sys.argv[5], "--bin", "tmux-agent-usage"]
    destination = pathlib.Path(os.environ["CARGO_TARGET_DIR"]) / sys.argv[5] / "release/tmux-agent-usage"
    destination.parent.mkdir(parents=True)
    shutil.copyfile(os.environ["RELEASE_FIXTURE_BINARY"], destination)
''')
        self.tool("rustc", 'print("rustc 1.89.0\\nhost: synthetic-test-fixture")\n')

    def tool(self, name, body):
        script = self.tools / name
        script.write_text("#!" + sys.executable + "\n" + body)
        script.chmod(0o755)

    def git(self, *args):
        return subprocess.check_output(["git", "-c", "user.name=Release tests", "-c", "user.email=tests@example.invalid",
                                        "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", *args],
                                       cwd=self.source, env=self.environment, text=True).strip()

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "Synthetic source fixture")

    def command(self, *args, failure=None):
        result = subprocess.run([sys.executable, str(self.source / "scripts/release.py"), *map(str, args)],
                                cwd=self.root, env=self.environment, capture_output=True, text=True)
        if failure:
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(failure, result.stderr)
        else:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def build(self, target, output, data=None):
        binary = self.root / "fixture binary"
        binary.write_bytes(data if data is not None else executable(target))
        self.environment["RELEASE_FIXTURE_BINARY"] = str(binary)
        self.command("build", "v0.1.0", target, output)
        return output / f"tmux-agent-usage-v0.1.0-{target}.tar.gz"

    def test_tag_clean_tree_and_source_root_are_checked_before_build(self):
        result = self.command("source", "v0.1.0")
        self.assertEqual(result.stdout.strip(), self.git("rev-parse", "HEAD"))
        self.command("build", "v0.2.0", release.TARGETS[0], self.root / "rejected", failure="tag must be vVERSION")
        (self.source / "untracked").write_text("must reject")
        self.command("source", "v0.1.0", failure="must be clean")
        self.commit()
        self.command("source", "v0.1.0", failure="tag must resolve to HEAD")
        self.assertFalse((self.root / "calls.jsonl").exists())
        self.assertFalse((self.root / "rejected").exists())
        self.git("tag", "-d", "v0.1.0")
        self.command("source", "v0.1.0", failure="refs/tags/v0.1.0")
        shutil.move(self.source / ".git", self.root / ".git")
        self.command("source", "v0.1.0", failure="must be the repository's Git root")

    def test_ci_checks_use_disposable_provider_paths_and_propagate_failure(self):
        self.tool("tmux", 'raise SystemExit("tmux must only run through isolated integration tests")\n')
        self.tool("cargo", '''import json, os, pathlib, sys
keys = ["HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "CODEX_HOME", "CLAUDE_CONFIG_DIR"]
with open(os.environ["RELEASE_FIXTURE_LOG"], "a") as log:
    log.write(json.dumps({"args": sys.argv[1:], "paths": {key: os.environ[key] for key in keys},
                         "token": os.getenv("CARGO_REGISTRY_TOKEN"), "tmux": os.getenv("TMUX")}) + "\\n")
assert pathlib.Path(os.environ["HOME"]).is_dir()
if sys.argv[1] == os.getenv("RELEASE_FIXTURE_FAILURE"):
    raise SystemExit(19)
''')
        log = self.root / "calls.jsonl"
        environment = dict(self.environment, CARGO_REGISTRY_TOKEN="synthetic-test-token", TMUX="synthetic-active-socket")
        for failure, expected in [("", 5), ("clippy", 2)]:
            environment["RELEASE_FIXTURE_FAILURE"] = failure
            log.unlink(missing_ok=True)
            result = subprocess.run(["sh", str(self.source / "scripts/ci.sh")], env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 19 if failure else 0, result.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(calls), expected)
            self.assertEqual([call["args"] for call in calls], [
                ["fmt", "--check"], ["clippy", "--locked", "--all-targets", "--", "-D", "warnings"],
                ["test", "--locked", "--", "--include-ignored"], ["package", "--locked", "--list"], ["publish", "--locked", "--dry-run"],
            ][:expected])
            for call in calls:
                self.assertIsNone(call["token"])
                self.assertIsNone(call["tmux"])
                paths = call["paths"]
                temporary_root = Path(paths["HOME"]).parent
                self.assertIn("agent-usage-ci.", temporary_root.name)
                self.assertTrue(all(Path(path).parent == temporary_root for path in paths.values()))
                self.assertFalse(temporary_root.exists(), "CI did not clean up its provider paths")

    def test_native_archive_matches_managed_installer_and_is_repeatable(self):
        native = ("aarch64" if platform.machine() in ("arm64", "aarch64") else "x86_64")
        native += "-apple-darwin" if sys.platform == "darwin" else "-unknown-linux-musl"
        data = Path(os.environ["AGENT_USAGE_TEST_BINARY"]).read_bytes()
        asset = self.build(native, self.root / "first", data)
        second = self.build(native, self.root / "second", data)
        self.assertEqual(asset.read_bytes(), second.read_bytes())
        self.assertEqual(asset.with_name(asset.name + ".sha256").read_text(),
                         f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n")
        with tarfile.open(asset) as archive:
            self.assertEqual(archive.getnames(), ["tmux-agent-usage"])
            member = archive.getmembers()[0]
            self.assertTrue(member.isreg())
            self.assertEqual((member.mode, member.uid, member.gid), (0o755, 0, 0))
            self.assertEqual(archive.extractfile(member).read(), data)
        provenance = json.loads(asset.with_name(asset.name + ".json").read_text())
        self.assertEqual(provenance["commit"], self.git("rev-parse", "HEAD"))
        self.assertEqual(provenance["repository"], "https://github.com/ramonmello/tmux-agent-usage")
        self.assertEqual(provenance["lockfile_sha256"], hashlib.sha256((self.source / "Cargo.lock").read_bytes()).hexdigest())
        self.tool("curl", '''import os, pathlib, shutil, sys
destination = pathlib.Path(sys.argv[sys.argv.index("--output") + 1])
url = sys.argv[-1]
assert url.startswith("https://github.com/ramonmello/tmux-agent-usage/releases/download/v0.1.0/")
shutil.copyfile(pathlib.Path(os.environ["RELEASE_FIXTURE_ASSETS"]) / url.rsplit("/", 1)[1], destination)
''')
        self.environment["RELEASE_FIXTURE_ASSETS"] = str(asset.parent)
        result = subprocess.run(["sh", str(self.source / "scripts/install-release.sh"), str(self.source)],
                                env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        installed = self.source / "bin/v0.1.0" / native / "tmux-agent-usage"
        self.assertEqual(installed.read_bytes(), data)
        result = subprocess.run([str(installed), "help"], env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("claude-tap", result.stdout)

    def test_all_target_assets_assemble_and_corruption_or_mixed_sources_fail(self):
        inputs = self.root / "inputs"
        inputs.mkdir()
        for target in release.TARGETS:
            self.build(target, inputs / target)
        missing = inputs / release.TARGETS[-1]
        saved = self.root / "withheld target"
        missing.rename(saved)
        self.command("assemble", "v0.1.0", inputs, self.root / "incomplete", failure="expected exactly one artifact")
        saved.rename(missing)
        shutil.copytree(missing, inputs / "duplicate")
        self.command("assemble", "v0.1.0", inputs, self.root / "duplicates", failure="expected exactly one artifact")
        shutil.rmtree(inputs / "duplicate")
        destination = self.root / "candidate-v0.1.0"
        self.command("assemble", "v0.1.0", inputs, destination)
        records = json.loads((destination / "provenance-v0.1.0.json").read_text())
        self.assertEqual([item["target"] for item in records["artifacts"]], list(release.TARGETS))
        checksums = (destination / "SHA256SUMS").read_text().splitlines()
        self.assertEqual(len(checksums), 13)
        for line in checksums:
            checksum, name = line.split("  ")
            self.assertEqual(hashlib.sha256((destination / name).read_bytes()).hexdigest(), checksum)
            self.assertIn("v0.1.0", name)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        for call in calls:
            if call["args"][0] == "build":
                self.assertIn("--locked", call["args"])
                self.assertEqual(call["rustflags"], "-C target-feature=+crt-static" if "linux" in call["args"][4] else "")
                self.assertEqual(call["deployment"], "11.0")
        archive = next(inputs.rglob("*.tar.gz"))
        original = archive.read_bytes()
        archive.write_bytes(original + b"tampered")
        self.command("assemble", "v0.1.0", inputs, self.root / "corrupt", failure="checksum mismatch")
        archive.write_bytes(original)
        provenance = archive.with_name(archive.name + ".json")
        record = json.loads(provenance.read_text())
        record["commit"] = "0" * 40
        provenance.write_text(json.dumps(record))
        self.command("assemble", "v0.1.0", inputs, self.root / "mixed", failure="artifact source mismatch")
        self.assertFalse((self.root / "corrupt").exists())
        self.assertFalse((self.root / "mixed").exists())

    def test_abi_rejects_wrong_arch_dynamic_linux_and_changed_macos_minimum(self):
        for target in release.TARGETS:
            release.check_binary(executable(target), target)
            other = next(item for item in release.TARGETS if item != target and item.split("-", 1)[1] == target.split("-", 1)[1])
            with self.assertRaisesRegex(ValueError, "architecture|Mach-O"):
                release.check_binary(executable(other), target)
        target = "x86_64-unknown-linux-musl"
        data = bytearray(executable(target))
        struct.pack_into("<I", data, 64, 3)
        with self.assertRaisesRegex(ValueError, "PT_INTERP"):
            release.check_binary(data, target)
        # Place DT_NEEDED after the program headers.
        data = bytearray(executable(target)) + bytearray(16)
        struct.pack_into("<I", data, 64, 2)
        struct.pack_into("<Q", data, 72, 128)
        struct.pack_into("<Q", data, 96, 16)
        struct.pack_into("<Q", data, 128, 1)
        with self.assertRaisesRegex(ValueError, "DT_NEEDED"):
            release.check_binary(data, target)
        data = bytearray(executable("aarch64-apple-darwin"))
        struct.pack_into("<I", data, 44, 15 << 16)
        with self.assertRaisesRegex(ValueError, "deployment target"):
            release.check_binary(data, "aarch64-apple-darwin")

    def test_build_refuses_overwrite_and_wrong_abi_without_partial_output(self):
        target = release.TARGETS[0]
        existing = self.root / "existing"
        existing.mkdir()
        (existing / "keep").write_text("unchanged")
        self.command("build", "v0.1.0", target, existing, failure="already exists")
        self.assertEqual((existing / "keep").read_text(), "unchanged")
        fixture = self.root / "bad binary"
        fixture.write_bytes(executable(release.TARGETS[1]))
        self.environment["RELEASE_FIXTURE_BINARY"] = str(fixture)
        self.command("build", "v0.1.0", target, self.root / "wrong", failure="Mach-O")
        self.assertFalse((self.root / "wrong").exists())
        self.assertFalse(list(self.root.glob(".agent-usage-release-*")))


if __name__ == "__main__":
    unittest.main(verbosity=2)

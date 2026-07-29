from __future__ import annotations

import argparse
import io
import json
import os
from pathlib import Path
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
from unittest import mock
import zipfile


REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "packaging"))

import devicerail_release as release  # noqa: E402


class ReleasePackagingTests(unittest.TestCase):
    # The packaging code cross-checks its cargo-metadata input against the real
    # workspace manifests, so the fixture version must be the live one.
    with (REPO_ROOT / "Cargo.toml").open("rb") as _cargo:
        version = tomllib.load(_cargo)["workspace"]["package"]["version"]

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="devicerail-release-test-")
        self.root = Path(self.temporary.name)
        self.binaries = self.root / "bin"
        self.binaries.mkdir()
        self.daemon = self._fake_binary("devicerail-daemon")
        self.bundle = self._fake_binary("devicerail-bundle")
        self.real_binary_version = release._binary_version
        self.real_binary_target = release._verify_binary_target
        self.binary_version_patch = mock.patch.object(release, "_binary_version")
        self.binary_version_patch.start()
        self.addCleanup(self.binary_version_patch.stop)
        self.binary_target_patch = mock.patch.object(release, "_verify_binary_target")
        self.binary_target_patch.start()
        self.addCleanup(self.binary_target_patch.stop)
        self.host_platform_patch = mock.patch.object(
            release,
            "_host_platform",
            return_value="linux",
        )
        self.host_platform_patch.start()
        self.addCleanup(self.host_platform_patch.stop)
        self.metadata = self.root / "metadata.json"
        self.metadata.write_text(
            json.dumps(
                {
                    "packages": [
                        {
                            "name": "devicerail-bundle-cli",
                            "version": self.version,
                            "license": None,
                            "source": None,
                            "checksum": None,
                            "targets": [{"name": "devicerail-bundle", "kind": ["bin"]}],
                        },
                        {
                            "name": "devicerail-daemon",
                            "version": self.version,
                            "license": None,
                            "source": None,
                            "checksum": None,
                            "targets": [{"name": "devicerail-daemon", "kind": ["bin"]}],
                        },
                        {
                            "name": "serde",
                            "version": "1.0.0",
                            "license": "MIT OR Apache-2.0",
                            "source": "registry+https://github.com/rust-lang/crates.io-index",
                            "checksum": "1" * 64,
                            "targets": [{"name": "serde", "kind": ["lib"]}],
                        },
                    ]
                },
                sort_keys=True,
            ),
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _fake_binary(self, name: str) -> Path:
        path = self.binaries / name
        path.write_text(
            "#!/bin/sh\n"
            'if [ "${1:-}" = "--version" ]; then\n'
            f"  printf '%s\\n' '{name} {self.version}'\n"
            "  exit 0\n"
            "fi\n"
            "exit 2\n",
            encoding="utf-8",
        )
        path.chmod(0o755)
        return path

    def _arguments(
        self,
        output: Path,
        *,
        platform: str = "linux",
        status: str = "unsigned-test-only",
        **overrides: object,
    ) -> argparse.Namespace:
        values: dict[str, object] = {
            "repo_root": REPO_ROOT,
            "platform": platform,
            "architecture": "x86_64",
            "daemon": self.daemon,
            "bundle": self.bundle,
            "output_dir": output,
            "source_date_epoch": 1_700_000_000,
            "release_status": status,
            "test_cargo_metadata": self.metadata,
            "source_uri": None,
            "linux_public_key": None,
            "linux_daemon_signature": None,
            "linux_bundle_signature": None,
            "artifact_public_key": None,
            "macos_team_id": None,
            "macos_designated_requirement": None,
            "macos_signing_identity": None,
            "windows_publisher_subject": None,
            "windows_publisher_sha256": None,
        }
        values.update(overrides)
        return argparse.Namespace(**values)

    def _package_unsigned(self, output: Path, *, platform: str = "linux") -> dict[str, object]:
        output.mkdir()
        return release.package_release(self._arguments(output, platform=platform))

    def _production_source_patch(self) -> mock._patch:
        return mock.patch.object(
            release,
            "_source_provenance",
            return_value=(
                {
                    "buildMode": "production-git",
                    "cargoLocked": True,
                    "sourceState": "clean",
                    "sourceMaterialComplete": True,
                },
                {
                    "uri": "git+https://example.invalid/device-rail.git",
                    "digest": {"sha1": "a" * 40},
                },
            ),
        )

    def _package_signed_linux(
        self,
        name: str,
    ) -> tuple[Path, Path, list[tuple[str, ...]]]:
        output = self.root / name
        output.mkdir()
        public_key = self.root / f"{name}.pub"
        daemon_signature = self.root / f"{name}-daemon.sig"
        bundle_signature = self.root / f"{name}-bundle.sig"
        public_key.write_bytes(b"test public key\n")
        daemon_signature.write_bytes(b"daemon signature\n")
        bundle_signature.write_bytes(b"bundle signature\n")
        calls: list[tuple[str, ...]] = []

        def successful_runner(arguments: list[str] | tuple[str, ...]) -> None:
            calls.append(tuple(arguments))

        with self._production_source_patch():
            summary = release.package_release(
                self._arguments(
                    output,
                    status="signed",
                    linux_public_key=public_key,
                    linux_daemon_signature=daemon_signature,
                    linux_bundle_signature=bundle_signature,
                    artifact_public_key=public_key,
                ),
                runner=successful_runner,
            )
        artifact = Path(summary["artifact"])
        artifact.with_name(f"{artifact.name}.sig").write_bytes(b"archive signature\n")
        return artifact, public_key, calls

    def test_unsigned_archive_is_reproducible_and_has_exact_contents(self) -> None:
        first = self._package_unsigned(self.root / "first")
        second = self._package_unsigned(self.root / "second")
        first_artifact = Path(first["artifact"])
        second_artifact = Path(second["artifact"])

        self.assertEqual(first_artifact.read_bytes(), second_artifact.read_bytes())
        self.assertEqual(
            first_artifact.with_name(f"{first_artifact.name}.provenance.json").read_bytes(),
            second_artifact.with_name(f"{second_artifact.name}.provenance.json").read_bytes(),
        )
        summary = release.verify_release(first_artifact)
        self.assertFalse(summary["authenticated"])
        self.assertEqual(summary["releaseStatus"], "unsigned-test-only")
        self.assertEqual(
            summary["binaryVersionVerification"],
            "package-contract-only-unsigned",
        )

        entries = release._read_archive(first_artifact)
        root = first_artifact.name.removesuffix(".tar.gz")
        relative = {name.removeprefix(f"{root}/") for name in entries}
        self.assertEqual(
            relative,
            {
                "BINARY-METADATA.json",
                "BUILD-PROVENANCE.intoto.json",
                "DISTRIBUTION-README.txt",
                "LICENSE",
                "LICENSES/THIRD-PARTY-LICENSES.md",
                "NOTICE",
                "SBOM.spdx.json",
                "SIGNING.json",
                "bin/devicerail-bundle",
                "bin/devicerail-daemon",
                "config/devicerail.env.example",
                "install.sh",
                "release-manifest.json",
            },
        )
        distribution_readme = entries[f"{root}/DISTRIBUTION-README.txt"][0].decode(
            "utf-8"
        )
        self.assertIn("WebDriverAgent (WDA)", distribution_readme)
        self.assertIn("iproxy", distribution_readme)
        self.assertIn("Appium/XCUITest Driver", distribution_readme)
        self.assertIn("only a booted Simulator is connected", distribution_readme)
        self.assertIn("does not create or boot Simulators", distribution_readme)
        self.assertIn(
            "DEVICERAIL_IOS_SESSION_TARGET=native|safari",
            distribution_readme,
        )
        self.assertIn(
            "DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS",
            distribution_readme,
        )
        self.assertIn(
            "HarmonyOS HDC discovery is disabled by default",
            distribution_readme,
        )
        self.assertIn("DEVICERAIL_HARMONY=required", distribution_readme)
        for text in (
            "DEVICERAIL_DESKTOP=auto",
            "DEVICERAIL_DESKTOP=required",
            "compile-time host",
            "Screen Recording and Accessibility (TCC)",
            "Session 0",
            "DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER=wayland",
            "Leaving display-server",
            "selection unset cannot bypass",
            "ydotoold",
            "/dev/uinput",
            "DEVICERAIL_DISTRIBUTED_PEERS",
            "DEVICERAIL_DISTRIBUTED_SERVER",
            "schemaVersion/nodeId/listen/securityMode/tunnelId/nodeEpoch/inventoryRevision",
            "externalSshOrMtls",
            "Raw loopback TCP has no built-in authentication",
            "not authenticate peer-v2",
            "starting gate",
            "node_starting",
            "not the ready transition",
            "Non-Unix owner-only configuration",
            "cross-host network validation",
        ):
            self.assertIn(text, distribution_readme)

        environment_example = entries[f"{root}/config/devicerail.env.example"][
            0
        ].decode("utf-8")
        self.assertIn("devicerail-daemon never loads it", environment_example)
        self.assertIn("an empty inventory", environment_example)
        self.assertIn(
            '# DEVICERAIL_IOS_DEVICE_NAME="Lab iPhone"',
            environment_example,
        )
        for variable in (
            "DEVICERAIL_IOS_BACKEND=direct-wda",
            "DEVICERAIL_IOS_BACKEND=appium",
            "DEVICERAIL_IOS_SESSION_TARGET=native",
            "DEVICERAIL_IOS_APPIUM_ENDPOINT",
            "DEVICERAIL_IOS_APPIUM_PATH",
            "DEVICERAIL_IOS_APPIUM_PORT",
            "DEVICERAIL_IOS_APPIUM_BASE_PATH",
            "DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS=600",
            "DEVICERAIL_DESKTOP=off",
            "DEVICERAIL_DESKTOP_ID",
            "DEVICERAIL_DESKTOP_NAME",
            "DEVICERAIL_DESKTOP_OS_VERSION",
            "DEVICERAIL_DESKTOP_COMMAND_TIMEOUT_MS",
            "DEVICERAIL_DESKTOP_MACOS_SCREENCAPTURE",
            "DEVICERAIL_DESKTOP_WINDOWS_POWERSHELL",
            "DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER",
            "DEVICERAIL_DESKTOP_X11_IMPORT",
            "DEVICERAIL_DESKTOP_X11_XDOTOOL",
            "DEVICERAIL_DESKTOP_WAYLAND_GRIM",
            "DEVICERAIL_DESKTOP_WAYLAND_INPUT",
            "DEVICERAIL_DESKTOP_WAYLAND_YDOTOOL",
            "DEVICERAIL_DESKTOP_WAYLAND_WTYPE",
            "DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_WIDTH",
            "DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_HEIGHT",
            "DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_SCALE_FACTOR",
            "DEVICERAIL_RPC_CREDENTIALS",
            "DEVICERAIL_RPC_AUDIT_LOG",
            "DEVICERAIL_PLUGIN_DIRS",
            "DEVICERAIL_PLUGIN_TIMEOUT_MS",
            "DEVICERAIL_DISTRIBUTED_PEERS",
            "DEVICERAIL_DISTRIBUTED_SERVER",
            'securityMode="externalSshOrMtls"',
            "nodeEpoch",
            "inventoryRevision",
            "Raw loopback TCP",
            "RPC credentials do not protect peer-v2",
            "starting gate permits discovery",
            "node_starting",
            "Non-Unix owner-only configuration currently fails closed",
        ):
            self.assertIn(variable, environment_example)
        provenance = json.loads(
            entries[f"{root}/BUILD-PROVENANCE.intoto.json"][0].decode("utf-8")
        )
        parameters = provenance["predicate"]["buildDefinition"]["externalParameters"]
        self.assertEqual(provenance["predicateType"], release.PROVENANCE_PREDICATE_TYPE)
        self.assertFalse(parameters["cargoLocked"])
        self.assertEqual(parameters["buildMode"], "test-fixture")
        self.assertFalse(parameters["sourceMaterialComplete"])

    def test_zip_traversal_member_is_rejected(self) -> None:
        archive = self.root / "malicious.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as target:
            info = zipfile.ZipInfo("../outside")
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            target.writestr(info, b"bad")
        with self.assertRaisesRegex(release.ReleaseError, "traverses"):
            release._read_archive(archive)

    def test_zip_entry_bomb_is_rejected_before_zipfile_allocation(self) -> None:
        archive = self.root / "entry-bomb.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as target:
            info = zipfile.ZipInfo("root/file")
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            target.writestr(info, b"ok")
        data = bytearray(archive.read_bytes())
        self.assertEqual(data[-22:-18], b"PK\x05\x06")
        data[-14:-12] = (release.MAX_ARCHIVE_ENTRIES + 1).to_bytes(2, "little")
        data[-12:-10] = (release.MAX_ARCHIVE_ENTRIES + 1).to_bytes(2, "little")
        archive.write_bytes(data)
        with mock.patch.object(
            release.zipfile,
            "ZipFile",
            side_effect=AssertionError("ZipFile must not be allocated"),
        ):
            with self.assertRaisesRegex(release.ReleaseError, "entry count"):
                release._read_archive(archive)

    def test_zip64_and_oversized_central_directory_are_rejected_in_preflight(self) -> None:
        archive = self.root / "preflight.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as target:
            info = zipfile.ZipInfo("root/file")
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            target.writestr(info, b"ok")
        original = archive.read_bytes()

        zip64 = bytearray(original)
        zip64[-14:-12] = (0xFFFF).to_bytes(2, "little")
        zip64[-12:-10] = (0xFFFF).to_bytes(2, "little")
        archive.write_bytes(zip64)
        with self.assertRaisesRegex(release.ReleaseError, "ZIP64"):
            release._read_archive(archive)

        oversized = bytearray(original)
        oversized[-10:-6] = (release.MAX_ZIP_CENTRAL_DIRECTORY_BYTES + 1).to_bytes(
            4,
            "little",
        )
        archive.write_bytes(oversized)
        with self.assertRaisesRegex(release.ReleaseError, "central directory"):
            release._read_archive(archive)

    def test_json_loader_rejects_non_utf8_and_non_finite_values(self) -> None:
        with self.assertRaisesRegex(release.ReleaseError, "UTF-8"):
            release._load_json_bytes("{}".encode("utf-16"), "fixture")
        with self.assertRaisesRegex(release.ReleaseError, "non-finite"):
            release._load_json_bytes(b'{"value":NaN}', "fixture")

    def test_manifest_schema_version_rejects_boolean_alias(self) -> None:
        summary = self._package_unsigned(self.root / "boolean-schema")
        artifact = Path(summary["artifact"])
        entries = release._read_archive(artifact)
        root = artifact.name.removesuffix(".tar.gz")
        path = f"{root}/release-manifest.json"
        manifest = json.loads(entries[path][0].decode("utf-8"))
        manifest["schemaVersion"] = True
        entries[path] = (release._canonical_json(manifest), entries[path][1])
        with self.assertRaisesRegex(release.ReleaseError, "schema"):
            release._validate_manifest(artifact, entries)

    def test_cargo_metadata_fixture_is_not_a_public_cli_option(self) -> None:
        arguments = [
            "package",
            "--platform",
            "linux",
            "--architecture",
            "x86_64",
            "--daemon",
            str(self.daemon),
            "--bundle",
            str(self.bundle),
            "--output-dir",
            str(self.root),
            "--source-date-epoch",
            "1700000000",
            "--cargo-metadata",
            str(self.metadata),
        ]
        with mock.patch("sys.stderr", new=io.StringIO()):
            with self.assertRaises(SystemExit):
                release._parser().parse_args(arguments)

    def test_git_source_provenance_rejects_dirty_signed_builds(self) -> None:
        repository = self.root / "source-repository"
        repository.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=repository, check=True)
        subprocess.run(
            ["git", "config", "user.email", "release@example.invalid"],
            cwd=repository,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Release Test"],
            cwd=repository,
            check=True,
        )
        tracked = repository / "tracked.txt"
        tracked.write_text("clean\n", encoding="utf-8")
        subprocess.run(["git", "add", "tracked.txt"], cwd=repository, check=True)
        subprocess.run(
            ["git", "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"],
            cwd=repository,
            check=True,
        )

        parameters, material = release._source_provenance(
            repository,
            signed=True,
            source_uri="git+https://example.invalid/device-rail.git",
            test_metadata_path=None,
        )
        self.assertEqual(parameters["sourceState"], "clean")
        self.assertTrue(parameters["cargoLocked"])
        self.assertEqual(material["uri"], "git+https://example.invalid/device-rail.git")
        self.assertIn(next(iter(material["digest"])), ("sha1", "sha256"))
        with self.assertRaisesRegex(release.ReleaseError, "explicit source repository URI"):
            release._source_provenance(
                repository,
                signed=True,
                source_uri=None,
                test_metadata_path=None,
            )

        tracked.write_text("dirty\n", encoding="utf-8")
        with self.assertRaisesRegex(release.ReleaseError, "clean git workspace"):
            release._source_provenance(
                repository,
                signed=True,
                source_uri="git+https://example.invalid/device-rail.git",
                test_metadata_path=None,
            )
        dirty, dirty_material = release._source_provenance(
            repository,
            signed=False,
            source_uri=None,
            test_metadata_path=None,
        )
        self.assertEqual(dirty["sourceState"], "dirty-uncommitted")
        self.assertFalse(dirty["sourceMaterialComplete"])
        self.assertRegex(dirty["workspaceStatusSha256"], r"^[0-9a-f]{64}$")
        self.assertEqual(dirty_material["uri"], "git+file://workspace")

    def test_executable_headers_must_match_the_release_target(self) -> None:
        elf = bytearray(64)
        elf[:6] = b"\x7fELF\x02\x01"
        elf[18:20] = (183).to_bytes(2, "little")
        self.real_binary_target(bytes(elf), "linux", "aarch64", "fixture")
        with self.assertRaisesRegex(release.ReleaseError, "does not match"):
            self.real_binary_target(bytes(elf), "linux", "x86_64", "fixture")

        macho = bytearray(32)
        macho[:4] = b"\xcf\xfa\xed\xfe"
        macho[4:8] = (0x01000007).to_bytes(4, "little")
        self.real_binary_target(bytes(macho), "macos", "x86_64", "fixture")

        pe = bytearray(128)
        pe[:2] = b"MZ"
        pe[0x3C:0x40] = (64).to_bytes(4, "little")
        pe[64:68] = b"PE\0\0"
        pe[68:70] = (0x8664).to_bytes(2, "little")
        self.real_binary_target(bytes(pe), "windows", "x86_64", "fixture")

    def test_tar_symlink_member_is_rejected(self) -> None:
        archive = self.root / "malicious.tar.gz"
        with tarfile.open(archive, "w:gz") as target:
            info = tarfile.TarInfo("root/bin/devicerail-daemon")
            info.type = tarfile.SYMTYPE
            info.linkname = "../../outside"
            target.addfile(info)
        with self.assertRaisesRegex(release.ReleaseError, "links"):
            release._read_archive(archive)

    def test_tar_entry_bomb_is_rejected_while_streaming(self) -> None:
        archive = self.root / "entry-bomb.tar.gz"
        with tarfile.open(archive, "w:gz") as target:
            for index in range(release.MAX_ARCHIVE_ENTRIES + 1):
                info = tarfile.TarInfo(f"root/file-{index:03d}")
                info.size = 0
                info.mode = 0o644
                target.addfile(info, io.BytesIO())
        with self.assertRaisesRegex(release.ReleaseError, "entry count"):
            release._read_archive(archive)

    def test_checksum_detects_archive_tampering(self) -> None:
        summary = self._package_unsigned(self.root / "checksum")
        artifact = Path(summary["artifact"])
        with artifact.open("ab") as target:
            target.write(b"tamper")
        with self.assertRaisesRegex(release.ReleaseError, "SHA-256 sidecar"):
            release.verify_release(artifact)

    def test_internal_manifest_detects_payload_tampering(self) -> None:
        summary = self._package_unsigned(self.root / "internal", platform="macos")
        artifact = Path(summary["artifact"])
        entries = release._read_archive(artifact)
        root = artifact.name.removesuffix(".zip")
        files = {
            name.removeprefix(f"{root}/"): value for name, value in entries.items()
        }
        daemon_path = "bin/devicerail-daemon"
        files[daemon_path] = (files[daemon_path][0] + b"tamper", files[daemon_path][1])
        artifact.unlink()
        release._write_zip(artifact, root, files, 1_700_000_000)
        digest, _size = release._sha256_file(artifact)
        artifact.with_name(f"{artifact.name}.sha256").write_text(
            f"{digest}  {artifact.name}\n",
            encoding="ascii",
        )
        with self.assertRaisesRegex(release.ReleaseError, "does not match its manifest"):
            release.verify_release(artifact)

    def test_verify_rechecks_embedded_executable_headers(self) -> None:
        summary = self._package_unsigned(self.root / "verify-header")
        artifact = Path(summary["artifact"])
        with mock.patch.object(
            release,
            "_verify_binary_target",
            self.real_binary_target,
        ):
            with self.assertRaisesRegex(release.ReleaseError, "executable target"):
                release.verify_release(artifact)

    def test_signed_verify_reexecutes_version_after_signature_trust(self) -> None:
        for path, name in (
            (self.daemon, "devicerail-daemon"),
            (self.bundle, "devicerail-bundle"),
        ):
            path.write_text(
                "#!/bin/sh\n"
                'if [ "${1:-}" = "--version" ]; then\n'
                f"  printf '%s\\n' '{name} 9.9.9'\n"
                "  exit 0\n"
                "fi\n"
                "exit 2\n",
                encoding="utf-8",
            )
            path.chmod(0o755)
        artifact, public_key, calls = self._package_signed_linux("wrong-version")

        def successful_runner(arguments: list[str] | tuple[str, ...]) -> None:
            calls.append(tuple(arguments))

        with mock.patch.object(release, "_binary_version", self.real_binary_version):
            with self.assertRaisesRegex(release.ReleaseError, "embedded version is inconsistent"):
                release.verify_release(
                    artifact,
                    trusted_artifact_public_key=public_key,
                    runner=successful_runner,
                )
        self.assertGreaterEqual(len(calls), 5)

    def test_signed_cross_platform_verify_fails_closed(self) -> None:
        artifact, public_key, calls = self._package_signed_linux("cross-platform")

        def successful_runner(arguments: list[str] | tuple[str, ...]) -> None:
            calls.append(tuple(arguments))

        with mock.patch.object(release, "_host_platform", return_value="macos"):
            with self.assertRaisesRegex(release.ReleaseError, "target operating system"):
                release.verify_release(
                    artifact,
                    trusted_artifact_public_key=public_key,
                    runner=successful_runner,
                )

    def test_macos_signature_requires_runtime_team_identity_and_requirement(self) -> None:
        team_id = "ABCDEFGHIJ"
        identity = "Developer ID Application: DeviceRail Test (ABCDEFGHIJ)"
        requirement = (
            'anchor apple generic and certificate leaf[subject.OU] = "ABCDEFGHIJ"'
        )
        inspection_text = (
            "Executable=/tmp/devicerail\n"
            "Identifier=devicerail\n"
            "CodeDirectory v=20500 size=100 flags=0x10000(runtime) hashes=1+0 location=embedded\n"
            f"Authority={identity}\n"
            f"TeamIdentifier={team_id}\n"
            "designated => identifier devicerail and anchor apple generic\n"
        )
        calls: list[tuple[str, ...]] = []

        def runner(arguments: list[str] | tuple[str, ...]) -> release.CommandOutput | None:
            calls.append(tuple(arguments))
            if "-d" in arguments:
                return release.CommandOutput(b"", inspection_text.encode("utf-8"))
            return None

        release._verify_payload_signatures(
            "macos",
            self.daemon,
            self.bundle,
            None,
            None,
            None,
            runner,
            macos_team_id=team_id,
            macos_designated_requirement=requirement,
            macos_signing_identity=identity,
        )
        self.assertEqual(len(calls), 4)
        self.assertEqual(
            [call for call in calls if "--verify" in call][0][-2],
            f"-R={requirement}",
        )

        variants = (
            inspection_text.replace("(runtime)", "(none)"),
            inspection_text.replace(team_id, "ZZZZZZZZZZ", 1),
            inspection_text.replace(f"Authority={identity}", "Authority=Unexpected"),
            inspection_text.replace(
                "designated => identifier devicerail and anchor apple generic",
                "Signature=adhoc\ndesignated => adhoc",
            ),
        )
        for invalid in variants:
            with self.subTest(invalid=invalid):
                def invalid_runner(
                    arguments: list[str] | tuple[str, ...],
                    output: str = invalid,
                ) -> release.CommandOutput | None:
                    if "-d" in arguments:
                        return release.CommandOutput(b"", output.encode("utf-8"))
                    return None

                with self.assertRaisesRegex(release.ReleaseError, "macOS signature lacks"):
                    release._verify_payload_signatures(
                        "macos",
                        self.daemon,
                        self.bundle,
                        None,
                        None,
                        None,
                        invalid_runner,
                        macos_team_id=team_id,
                        macos_designated_requirement=requirement,
                        macos_signing_identity=identity,
                    )

    def test_signed_macos_manifest_and_verify_bind_out_of_band_identity(self) -> None:
        output = self.root / "signed-macos"
        output.mkdir()
        public_key = self.root / "signed-macos.pub"
        public_key.write_bytes(b"archive key\n")
        team_id = "ABCDEFGHIJ"
        identity = "Developer ID Application: DeviceRail Test (ABCDEFGHIJ)"
        requirement = (
            'anchor apple generic and certificate leaf[subject.OU] = "ABCDEFGHIJ"'
        )
        inspection = release.CommandOutput(
            b"",
            (
                "CodeDirectory v=20500 size=100 flags=0x10000(runtime) hashes=1+0 location=embedded\n"
                f"Authority={identity}\n"
                f"TeamIdentifier={team_id}\n"
                "designated => identifier devicerail and anchor apple generic\n"
            ).encode("utf-8"),
        )

        def runner(arguments: list[str] | tuple[str, ...]) -> release.CommandOutput | None:
            if "-d" in arguments:
                return inspection
            return None

        with self._production_source_patch():
            summary = release.package_release(
                self._arguments(
                    output,
                    platform="macos",
                    status="signed",
                    artifact_public_key=public_key,
                    macos_team_id=team_id,
                    macos_designated_requirement=requirement,
                    macos_signing_identity=identity,
                ),
                runner=runner,
            )
        artifact = Path(summary["artifact"])
        artifact.with_name(f"{artifact.name}.sig").write_bytes(b"archive signature\n")
        with mock.patch.object(release, "_host_platform", return_value="macos"):
            verified = release.verify_release(
                artifact,
                trusted_artifact_public_key=public_key,
                runner=runner,
                expected_macos_team_id=team_id,
                expected_macos_designated_requirement=requirement,
                expected_macos_signing_identity=identity,
            )
        self.assertTrue(verified["authenticated"])
        with self.assertRaisesRegex(release.ReleaseError, "out-of-band expected identity"):
            release.verify_release(
                artifact,
                trusted_artifact_public_key=public_key,
                runner=runner,
                expected_macos_team_id=team_id,
                expected_macos_designated_requirement=requirement,
                expected_macos_signing_identity="Developer ID Application: Unexpected",
            )

    def test_signed_release_requires_and_dispatches_all_verifiers(self) -> None:
        output = self.root / "signed"
        output.mkdir()
        public_key = self.root / "cosign.pub"
        daemon_signature = self.root / "daemon.sig"
        bundle_signature = self.root / "bundle.sig"
        for path, data in (
            (public_key, b"test public key\n"),
            (daemon_signature, b"daemon signature\n"),
            (bundle_signature, b"bundle signature\n"),
        ):
            path.write_bytes(data)

        calls: list[tuple[str, ...]] = []

        def successful_runner(arguments: list[str] | tuple[str, ...]) -> None:
            calls.append(tuple(arguments))

        with self._production_source_patch():
            summary = release.package_release(
                self._arguments(
                    output,
                    status="signed",
                    linux_public_key=public_key,
                    linux_daemon_signature=daemon_signature,
                    linux_bundle_signature=bundle_signature,
                    artifact_public_key=public_key,
                ),
                runner=successful_runner,
            )
        artifact = Path(summary["artifact"])
        artifact.with_name(f"{artifact.name}.sig").write_bytes(b"archive signature\n")
        with self.assertRaisesRegex(release.ReleaseError, "out-of-band trusted public key"):
            release.verify_release(artifact, runner=successful_runner)
        verified = release.verify_release(
            artifact,
            trusted_artifact_public_key=public_key,
            runner=successful_runner,
        )
        self.assertTrue(verified["authenticated"])
        self.assertEqual(len(calls), 5)
        self.assertTrue(all(call[0] == "cosign" for call in calls))

        def failing_runner(_arguments: list[str] | tuple[str, ...]) -> None:
            raise release.ReleaseError("signature verifier rejected input")

        with self.assertRaisesRegex(release.ReleaseError, "signature verifier rejected"):
            release.verify_release(
                artifact,
                trusted_artifact_public_key=public_key,
                runner=failing_runner,
            )

    def test_signed_payload_is_authenticated_before_version_execution(self) -> None:
        output = self.root / "signature-before-execution"
        output.mkdir()
        public_key = self.root / "order.pub"
        daemon_signature = self.root / "order-daemon.sig"
        bundle_signature = self.root / "order-bundle.sig"
        for path in (public_key, daemon_signature, bundle_signature):
            path.write_bytes(b"fixture\n")
        events: list[str] = []

        def runner(_arguments: list[str] | tuple[str, ...]) -> None:
            events.append("signature")

        def binary_version(_binary: Path, _name: str, _version: str) -> None:
            events.append("version")

        with self._production_source_patch(), mock.patch.object(
            release,
            "_binary_version",
            side_effect=binary_version,
        ):
            release.package_release(
                self._arguments(
                    output,
                    status="signed",
                    linux_public_key=public_key,
                    linux_daemon_signature=daemon_signature,
                    linux_bundle_signature=bundle_signature,
                    artifact_public_key=public_key,
                ),
                runner=runner,
            )
        self.assertEqual(events, ["signature", "signature", "version", "version"])

    def test_windows_signing_identity_is_bound_out_of_band(self) -> None:
        output = self.root / "signed-windows"
        output.mkdir()
        public_key = self.root / "windows-archive.pub"
        public_key.write_bytes(b"archive key\n")
        subject = "CN=DeviceRail Release, O=DeviceRail"
        thumbprint = "b" * 64
        calls: list[tuple[str, ...]] = []

        def runner(arguments: list[str] | tuple[str, ...]) -> release.CommandOutput | None:
            calls.append(tuple(arguments))
            if arguments[0] == "powershell.exe":
                return release.CommandOutput(
                    f"{subject}\n{thumbprint}\n".encode("utf-8"),
                    b"",
                )
            return None

        with self._production_source_patch():
            summary = release.package_release(
                self._arguments(
                    output,
                    platform="windows",
                    status="signed",
                    artifact_public_key=public_key,
                    windows_publisher_subject=subject,
                    windows_publisher_sha256=thumbprint.upper(),
                ),
                runner=runner,
            )
        artifact = Path(summary["artifact"])
        artifact.with_name(f"{artifact.name}.sig").write_bytes(b"archive signature\n")
        with mock.patch.object(release, "_host_platform", return_value="windows"):
            verified = release.verify_release(
                artifact,
                trusted_artifact_public_key=public_key,
                runner=runner,
                expected_windows_publisher_subject=subject,
                expected_windows_publisher_sha256=thumbprint,
            )
        self.assertTrue(verified["authenticated"])
        self.assertEqual(sum(call[0] == "signtool" for call in calls), 4)
        self.assertEqual(sum(call[0] == "powershell.exe" for call in calls), 4)
        with self.assertRaisesRegex(release.ReleaseError, "out-of-band expected identity"):
            release.verify_release(
                artifact,
                trusted_artifact_public_key=public_key,
                runner=runner,
                expected_windows_publisher_subject="CN=Unexpected",
                expected_windows_publisher_sha256=thumbprint,
            )

    def test_signed_verification_rejects_test_fixture_provenance(self) -> None:
        summary = self._package_unsigned(self.root / "fixture-provenance")
        artifact = Path(summary["artifact"])
        entries = release._read_archive(artifact)
        _root, manifest, payload = release._validate_manifest(artifact, entries)
        signed_manifest = dict(manifest)
        signed_manifest["releaseStatus"] = "signed"
        fixture_parameters = {
            "buildMode": "test-fixture",
            "cargoLocked": False,
            "sourceState": "test-fixture",
            "sourceMaterialComplete": False,
        }
        fixture_material = {
            "uri": "test-fixture:cargo-metadata",
            "digest": {"sha256": "c" * 64},
        }
        with mock.patch.object(
            release,
            "_validate_manifest",
            return_value=("root", signed_manifest, payload),
        ), mock.patch.object(
            release,
            "_validate_embedded_metadata",
            return_value=(fixture_parameters, fixture_material),
        ):
            with self.assertRaisesRegex(release.ReleaseError, "production-git provenance"):
                release.verify_release(artifact)

    def test_signed_configuration_is_platform_complete_and_unambiguous(self) -> None:
        output = self.root / "incomplete-signed"
        output.mkdir()
        public_key = self.root / "archive.pub"
        public_key.write_bytes(b"public key\n")
        with self.assertRaisesRegex(release.ReleaseError, "production-git provenance"):
            release.package_release(
                self._arguments(
                    output,
                    status="signed",
                    artifact_public_key=public_key,
                )
            )
        linux_signature = self.root / "irrelevant.sig"
        linux_signature.write_bytes(b"signature\n")
        with self._production_source_patch():
            with self.assertRaisesRegex(release.ReleaseError, "signed Linux"):
                release.package_release(
                    self._arguments(
                        output,
                        status="signed",
                        artifact_public_key=public_key,
                    )
                )
            with self.assertRaisesRegex(release.ReleaseError, "invalid for this platform"):
                release.package_release(
                    self._arguments(
                        output,
                        platform="macos",
                        status="signed",
                        artifact_public_key=public_key,
                        linux_public_key=public_key,
                        linux_daemon_signature=linux_signature,
                        linux_bundle_signature=linux_signature,
                    )
                )
            with self.assertRaisesRegex(release.ReleaseError, "expected Team ID"):
                release.package_release(
                    self._arguments(
                        output,
                        platform="macos",
                        status="signed",
                        artifact_public_key=public_key,
                        macos_designated_requirement="anchor apple generic",
                        macos_signing_identity="Developer ID Application: DeviceRail",
                    )
                )

    def test_unsigned_release_rejects_ambiguous_signature_sidecar(self) -> None:
        summary = self._package_unsigned(self.root / "ambiguous")
        artifact = Path(summary["artifact"])
        artifact.with_name(f"{artifact.name}.sig").write_bytes(b"not a release signature")
        with self.assertRaisesRegex(release.ReleaseError, "ambiguous"):
            release.verify_release(artifact)

    def test_version_consistency_rejects_metadata_mismatch(self) -> None:
        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        metadata["packages"][0]["version"] = "9.9.9"
        with self.assertRaisesRegex(release.ReleaseError, "inconsistent"):
            release._workspace_version(REPO_ROOT, metadata)

    def test_semver_source_epoch_and_spdx_namespace_are_strict(self) -> None:
        accepted = ("0.1.0", "1.2.3-alpha.1+build.7", "1.0.0-0")
        rejected = ("01.2.3", "1.02.3", "1.2.3-01", "1.2", "1.2.3+")
        for value in accepted:
            self.assertIsNotNone(release.VERSION_PATTERN.fullmatch(value))
        for value in rejected:
            self.assertIsNone(release.VERSION_PATTERN.fullmatch(value))
        self.assertEqual(
            release._validate_source_date_epoch(
                release.MAX_GZIP_SOURCE_DATE_EPOCH,
                "linux",
            ),
            release.MAX_GZIP_SOURCE_DATE_EPOCH,
        )
        with self.assertRaisesRegex(release.ReleaseError, "gzip timestamp range"):
            release._validate_source_date_epoch(
                release.MAX_GZIP_SOURCE_DATE_EPOCH + 1,
                "linux",
            )
        with self.assertRaisesRegex(release.ReleaseError, "ZIP timestamp range"):
            release._validate_source_date_epoch(
                release.MAX_ZIP_SOURCE_DATE_EPOCH + 1,
                "windows",
            )

        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        identity = {
            "sourceMaterial": {
                "uri": "git+https://example.invalid/device-rail.git",
                "digest": {"sha1": "d" * 40},
            },
            "binaries": [{"name": "devicerail-daemon", "sha256": "e" * 64}],
        }
        first, _inventory = release._build_sbom(
            metadata,
            self.version,
            "linux",
            "x86_64",
            1_700_000_000,
            identity,
        )
        repeated, _inventory = release._build_sbom(
            metadata,
            self.version,
            "linux",
            "x86_64",
            1_700_000_000,
            identity,
        )
        changed, _inventory = release._build_sbom(
            metadata,
            self.version,
            "linux",
            "x86_64",
            1_700_000_000,
            {**identity, "binaries": [{"name": "devicerail-daemon", "sha256": "f" * 64}]},
        )
        self.assertEqual(first, repeated)
        self.assertNotEqual(
            json.loads(first)["documentNamespace"],
            json.loads(changed)["documentNamespace"],
        )

    def test_tar_decompressed_stream_and_provenance_shape_are_bounded(self) -> None:
        archive = self.root / "bounded.tar.gz"
        with tarfile.open(archive, "w:gz") as target:
            info = tarfile.TarInfo("root/file")
            info.size = 2048
            info.mode = 0o644
            target.addfile(info, io.BytesIO(b"x" * info.size))
        with mock.patch.object(release, "MAX_TAR_STREAM_BYTES", 1024):
            with self.assertRaisesRegex(release.ReleaseError, "stream limit"):
                release._read_archive(archive)

        pax_archive = self.root / "bounded-pax.tar.gz"
        with tarfile.open(pax_archive, "w:gz", format=tarfile.PAX_FORMAT) as target:
            info = tarfile.TarInfo("root/file")
            info.mode = 0o644
            info.pax_headers = {"comment": "x" * 4096}
            target.addfile(info, io.BytesIO())
        with mock.patch.object(release, "MAX_TAR_STREAM_BYTES", 1024):
            with self.assertRaisesRegex(release.ReleaseError, "stream limit"):
                release._read_archive(pax_archive)

        date = release._source_date(1_700_000_000)
        statement = {
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [],
            "predicateType": release.PROVENANCE_PREDICATE_TYPE,
            "predicate": {
                "buildDefinition": {
                    "buildType": "https://devicerail.dev/build-types/cargo-release-v1",
                    "externalParameters": {},
                    "internalParameters": {},
                    "resolvedDependencies": [],
                },
                "runDetails": {
                    "builder": {"id": release.PROVENANCE_BUILDER_ID},
                    "metadata": {
                        "invocationId": "",
                        "startedOn": date,
                        "finishedOn": date,
                    },
                },
            },
        }
        release._validated_provenance_definition(
            statement,
            expected_build_type="https://devicerail.dev/build-types/cargo-release-v1",
            source_date_epoch=1_700_000_000,
            label="fixture",
        )
        statement["unexpected"] = True
        with self.assertRaisesRegex(release.ReleaseError, "shape or type"):
            release._validated_provenance_definition(
                statement,
                expected_build_type="https://devicerail.dev/build-types/cargo-release-v1",
                source_date_epoch=1_700_000_000,
                label="fixture",
            )


if __name__ == "__main__":
    unittest.main()

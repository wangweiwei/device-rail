#!/usr/bin/env python3
"""Build and verify deterministic DeviceRail portable release archives.

The normal unsigned path uses only the Python standard library and Cargo
metadata already present in the workspace. Production signing is deliberately
delegated to native platform tools and cosign; a signed claim is rejected when
any required verifier or signature is absent.
"""

from __future__ import annotations

import argparse
import datetime as dt
from dataclasses import dataclass
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from typing import Any, Callable, Iterable, Mapping, Sequence
from urllib.parse import urlsplit
import zipfile


SCHEMA_VERSION = 1
MAX_BINARY_BYTES = 256 * 1024 * 1024
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_ARCHIVE_ENTRIES = 64
MAX_TEXT_BYTES = 1024 * 1024
MAX_COMMAND_OUTPUT_BYTES = 64 * 1024
MAX_ZIP_CENTRAL_DIRECTORY_BYTES = MAX_ARCHIVE_ENTRIES * (46 + 240)
MAX_TAR_STREAM_BYTES = MAX_ARCHIVE_BYTES + 2 * 1024 * 1024
MAX_GZIP_SOURCE_DATE_EPOCH = 4_294_967_295
MAX_ZIP_SOURCE_DATE_EPOCH = 4_354_819_199
PLATFORMS = ("linux", "macos", "windows")
ARCHITECTURES = ("x86_64", "aarch64")
SEMVER_NUMBER = r"(?:0|[1-9][0-9]*)"
SEMVER_PRERELEASE_IDENTIFIER = (
    r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
)
VERSION_PATTERN = re.compile(
    rf"{SEMVER_NUMBER}\.{SEMVER_NUMBER}\.{SEMVER_NUMBER}"
    rf"(?:-{SEMVER_PRERELEASE_IDENTIFIER}(?:\.{SEMVER_PRERELEASE_IDENTIFIER})*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z"
)
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}\Z")
CERTIFICATE_SHA256_PATTERN = re.compile(r"[0-9A-Fa-f]{64}\Z")
GIT_COMMIT_PATTERN = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
PROVENANCE_PREDICATE_TYPE = "https://devicerail.dev/provenance/v1"
PROVENANCE_BUILDER_ID = "https://devicerail.dev/release-packager/v1"
WINDOWS_SIGNATURE_INSPECTION = (
    "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); "
    "$signature = Get-AuthenticodeSignature -LiteralPath $args[0]; "
    "if ($null -eq $signature.SignerCertificate -or "
    "$signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) { exit 1 }; "
    "$subject = $signature.SignerCertificate.Subject; "
    "$sha256 = $signature.SignerCertificate.GetCertHashString("
    "[System.Security.Cryptography.HashAlgorithmName]::SHA256).ToLowerInvariant(); "
    "[Console]::Out.Write($subject + \"`n\" + $sha256 + \"`n\")"
)


class ReleaseError(Exception):
    """A bounded, user-safe release construction or verification failure."""


class _BoundedReadStream:
    """Expose at most ``limit`` decompressed bytes from a readable stream."""

    def __init__(self, source: Any, limit: int, label: str) -> None:
        self._source = source
        self._remaining = limit
        self._label = label

    def read(self, size: int = -1) -> bytes:
        if size is None or size < 0:
            size = self._remaining + 1
        else:
            size = min(size, self._remaining + 1)
        data = self._source.read(size)
        if len(data) > self._remaining:
            raise ReleaseError(f"{self._label} expands beyond its stream limit")
        self._remaining -= len(data)
        return data


@dataclass(frozen=True)
class CommandOutput:
    stdout: bytes
    stderr: bytes


Runner = Callable[[Sequence[str]], CommandOutput | None]


def _canonical_json(value: Any) -> bytes:
    return (
        json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode("utf-8")


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ReleaseError("JSON contains duplicate object keys")
        result[key] = value
    return result


def _load_json_bytes(data: bytes, label: str) -> Any:
    if len(data) > MAX_TEXT_BYTES:
        raise ReleaseError(f"{label} exceeds the text size limit")

    def reject_constant(_value: str) -> None:
        raise ReleaseError(f"{label} contains a non-finite number")

    try:
        text = data.decode("utf-8", errors="strict")
        return json.loads(
            text,
            object_pairs_hook=_reject_duplicate_keys,
            parse_constant=reject_constant,
        )
    except ReleaseError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReleaseError(f"{label} is not strict UTF-8 JSON") from error


def _load_json_file(path: Path, label: str) -> Any:
    return _load_json_bytes(_read_regular_file(path, MAX_TEXT_BYTES, label), label)


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _sha256_file(path: Path, maximum: int = MAX_ARCHIVE_BYTES) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    try:
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                size += len(chunk)
                if size > maximum:
                    raise ReleaseError(f"{path.name} exceeds the size limit")
                digest.update(chunk)
    except OSError as error:
        raise ReleaseError(f"could not read {path.name}") from error
    return digest.hexdigest(), size


def _read_regular_file(path: Path, maximum: int, label: str) -> bytes:
    try:
        before = path.lstat()
    except OSError as error:
        raise ReleaseError(f"{label} is missing") from error
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise ReleaseError(f"{label} must be a regular file, not a link")
    if before.st_size > maximum:
        raise ReleaseError(f"{label} exceeds the size limit")

    flags = os.O_RDONLY
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as source:
            opened = os.fstat(source.fileno())
            if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                raise ReleaseError(f"{label} changed while it was opened")
            data = source.read(maximum + 1)
            after = os.fstat(source.fileno())
            if (
                (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
                != (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
                or len(data) != after.st_size
            ):
                raise ReleaseError(f"{label} changed while it was read")
    except ReleaseError:
        raise
    except OSError as error:
        raise ReleaseError(f"could not safely read {label}") from error
    if len(data) > maximum:
        raise ReleaseError(f"{label} exceeds the size limit")
    return data


def _safe_relative_path(value: str) -> PurePosixPath:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > 240:
        raise ReleaseError("archive member path is empty or too long")
    if "\\" in value or "\x00" in value or not value.isascii():
        raise ReleaseError("archive member path is not portable ASCII")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise ReleaseError("archive member path is absolute or traverses its root")
    if path.as_posix() != value:
        raise ReleaseError("archive member path is not canonical")
    return path


def _source_date(source_date_epoch: int) -> str:
    try:
        return dt.datetime.fromtimestamp(source_date_epoch, tz=dt.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        )
    except (OverflowError, OSError, ValueError) as error:
        raise ReleaseError("SOURCE_DATE_EPOCH is outside the supported range") from error


def _validate_source_date_epoch(source_date_epoch: Any, platform: str) -> int:
    if (
        isinstance(source_date_epoch, bool)
        or not isinstance(source_date_epoch, int)
        or source_date_epoch < 0
    ):
        raise ReleaseError("SOURCE_DATE_EPOCH must be a non-negative integer")
    maximum = (
        MAX_GZIP_SOURCE_DATE_EPOCH
        if platform == "linux"
        else MAX_ZIP_SOURCE_DATE_EPOCH
    )
    if source_date_epoch > maximum:
        archive_kind = "gzip" if platform == "linux" else "ZIP"
        raise ReleaseError(
            f"SOURCE_DATE_EPOCH exceeds the {archive_kind} timestamp range"
        )
    _source_date(source_date_epoch)
    return source_date_epoch


def _workspace_version(repo_root: Path, cargo_metadata: Mapping[str, Any]) -> str:
    try:
        with (repo_root / "Cargo.toml").open("rb") as source:
            cargo = tomllib.load(source)
        cargo_version = cargo["workspace"]["package"]["version"]
        package_version = _load_json_file(repo_root / "package.json", "package.json")[
            "version"
        ]
    except (OSError, KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise ReleaseError("workspace version metadata is missing or malformed") from error
    if not isinstance(cargo_version, str) or not VERSION_PATTERN.fullmatch(cargo_version):
        raise ReleaseError("Cargo workspace version is not a safe semantic version")
    if package_version != cargo_version:
        raise ReleaseError("Cargo.toml and package.json versions differ")

    expected = {
        "devicerail-daemon": "devicerail-daemon",
        "devicerail-bundle-cli": "devicerail-bundle",
    }
    found: dict[str, tuple[str, set[str]]] = {}
    packages = cargo_metadata.get("packages")
    if not isinstance(packages, list):
        raise ReleaseError("Cargo metadata does not contain packages")
    for package in packages:
        if not isinstance(package, dict) or package.get("name") not in expected:
            continue
        targets = package.get("targets", [])
        target_names = {
            target.get("name")
            for target in targets
            if isinstance(target, dict)
            and isinstance(target.get("kind"), list)
            and "bin" in target["kind"]
        }
        found[package["name"]] = (package.get("version"), target_names)
    for package_name, binary_name in expected.items():
        version_and_targets = found.get(package_name)
        if version_and_targets is None:
            raise ReleaseError(f"Cargo metadata is missing {package_name}")
        version, targets = version_and_targets
        if version != cargo_version or binary_name not in targets:
            raise ReleaseError(f"{package_name} version or binary target is inconsistent")
    return cargo_version


def _cargo_metadata(repo_root: Path, metadata_path: Path | None) -> Mapping[str, Any]:
    if metadata_path is not None:
        value = _load_json_file(metadata_path, "Cargo metadata fixture")
    else:
        try:
            result = subprocess.run(
                [
                    "cargo",
                    "metadata",
                    "--locked",
                    "--offline",
                    "--no-deps",
                    "--format-version",
                    "1",
                ],
                cwd=repo_root,
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=60,
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise ReleaseError("cargo metadata --locked --offline --no-deps failed") from error
        value = _load_json_bytes(result.stdout, "Cargo metadata")
        try:
            with (repo_root / "Cargo.lock").open("rb") as source:
                lockfile = tomllib.load(source)
        except (OSError, tomllib.TOMLDecodeError) as error:
            raise ReleaseError("Cargo.lock is missing or malformed") from error
        if not isinstance(value, dict) or not isinstance(value.get("packages"), list):
            raise ReleaseError("Cargo metadata root must contain packages")
        existing = {
            (package.get("name"), package.get("version"), package.get("source"))
            for package in value["packages"]
            if isinstance(package, dict)
        }
        locked_packages = lockfile.get("package")
        if not isinstance(locked_packages, list):
            raise ReleaseError("Cargo.lock does not contain packages")
        for package in locked_packages:
            if not isinstance(package, dict):
                raise ReleaseError("Cargo.lock package entry is malformed")
            identity = (package.get("name"), package.get("version"), package.get("source"))
            if identity in existing:
                continue
            name, package_version, source = identity
            checksum = package.get("checksum")
            if (
                not isinstance(name, str)
                or not isinstance(package_version, str)
                or (source is not None and not isinstance(source, str))
                or (checksum is not None and not isinstance(checksum, str))
            ):
                raise ReleaseError("Cargo.lock package identity is malformed")
            value["packages"].append(
                {
                    "name": name,
                    "version": package_version,
                    "license": None,
                    "source": source,
                    "checksum": checksum,
                    "targets": [],
                }
            )
            existing.add(identity)
    if not isinstance(value, dict):
        raise ReleaseError("Cargo metadata root must be an object")
    return value


def _git_output(repo_root: Path, arguments: Sequence[str], label: str) -> bytes:
    try:
        result = subprocess.run(
            ["git", *arguments],
            cwd=repo_root,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ReleaseError(f"could not determine {label} from git") from error
    if (
        len(result.stdout) > MAX_COMMAND_OUTPUT_BYTES
        or len(result.stderr) > MAX_COMMAND_OUTPUT_BYTES
    ):
        raise ReleaseError(f"git output exceeded its limit while reading {label}")
    return result.stdout


def _validate_source_uri(value: str | None, *, signed: bool) -> str:
    if value is None:
        if signed:
            raise ReleaseError("signed releases require an explicit source repository URI")
        return "git+file://workspace"
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > 512
        or not value.isascii()
        or any(ord(character) < 0x20 or ord(character) == 0x7F for character in value)
    ):
        raise ReleaseError("source repository URI is not bounded portable ASCII")
    try:
        parsed = urlsplit(value)
        hostname = parsed.hostname
        password = parsed.password
        username = parsed.username
    except ValueError as error:
        raise ReleaseError("source repository URI is malformed") from error
    if (
        parsed.scheme not in ("git+https", "git+ssh")
        or not hostname
        or parsed.query
        or parsed.fragment
        or password is not None
        or (username is not None and username != "git")
        or not parsed.path
        or parsed.path == "/"
    ):
        raise ReleaseError(
            "source repository URI must be a credential-free git HTTPS or SSH URI"
        )
    return value


def _source_provenance(
    repo_root: Path,
    *,
    signed: bool,
    source_uri: str | None,
    test_metadata_path: Path | None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Return honest source parameters and the single source material descriptor."""

    if test_metadata_path is not None:
        if source_uri is not None:
            raise ReleaseError("Cargo metadata fixtures cannot claim a source repository URI")
        fixture = _read_regular_file(
            test_metadata_path,
            MAX_TEXT_BYTES,
            "Cargo metadata fixture",
        )
        return (
            {
                "buildMode": "test-fixture",
                "cargoLocked": False,
                "sourceState": "test-fixture",
                "sourceMaterialComplete": False,
            },
            {
                "uri": "test-fixture:cargo-metadata",
                "digest": {"sha256": _sha256_bytes(fixture)},
            },
        )

    commit_bytes = _git_output(repo_root, ["rev-parse", "HEAD"], "source commit")
    try:
        commit = commit_bytes.decode("ascii", errors="strict").strip()
    except UnicodeDecodeError as error:
        raise ReleaseError("git source commit is not ASCII") from error
    if not GIT_COMMIT_PATTERN.fullmatch(commit):
        raise ReleaseError("git source commit is malformed")
    status = _git_output(
        repo_root,
        ["status", "--porcelain=v1", "--untracked-files=all"],
        "workspace status",
    )
    dirty = bool(status)
    if signed and dirty:
        raise ReleaseError("signed releases require a clean git workspace")
    parameters: dict[str, Any] = {
        "buildMode": "production-git",
        "cargoLocked": True,
        "sourceState": "dirty-uncommitted" if dirty else "clean",
        "sourceMaterialComplete": not dirty,
    }
    if dirty:
        parameters["workspaceStatusSha256"] = _sha256_bytes(status)
    algorithm = "sha1" if len(commit) == 40 else "sha256"
    return (
        parameters,
        {
            "uri": _validate_source_uri(source_uri, signed=signed),
            "digest": {algorithm: commit},
        },
    )


def _require_signed_source_claim(
    parameters: Mapping[str, Any], material: Mapping[str, Any]
) -> None:
    expected = {
        "buildMode": "production-git",
        "cargoLocked": True,
        "sourceState": "clean",
        "sourceMaterialComplete": True,
    }
    if dict(parameters) != expected:
        raise ReleaseError(
            "signed releases require complete clean production-git provenance"
        )
    uri = material.get("uri")
    if not isinstance(uri, str):
        raise ReleaseError("signed release source material URI is missing")
    _validate_source_uri(uri, signed=True)


def _spdx_id(name: str, index: int) -> str:
    normalized = re.sub(r"[^A-Za-z0-9.-]", "-", name)
    return f"SPDXRef-Package-{normalized}-{index}"


def _build_sbom(
    metadata: Mapping[str, Any],
    version: str,
    platform: str,
    architecture: str,
    epoch: int,
    release_identity: Mapping[str, Any],
) -> tuple[bytes, bytes]:
    raw_packages = metadata.get("packages", [])
    selected: list[tuple[str, str, str | None, str | None, str | None]] = []
    for package in raw_packages:
        if not isinstance(package, dict):
            raise ReleaseError("Cargo metadata package entry must be an object")
        name = package.get("name")
        package_version = package.get("version")
        if not isinstance(name, str) or not isinstance(package_version, str):
            raise ReleaseError("Cargo metadata package identity is malformed")
        license_expression = package.get("license")
        source = package.get("source")
        checksum = package.get("checksum")
        if license_expression is not None and not isinstance(license_expression, str):
            raise ReleaseError("Cargo metadata license is malformed")
        if source is not None and not isinstance(source, str):
            raise ReleaseError("Cargo metadata source is malformed")
        if checksum is not None and not isinstance(checksum, str):
            raise ReleaseError("Cargo metadata checksum is malformed")
        selected.append((name, package_version, license_expression, source, checksum))
    selected.sort(key=lambda item: (item[0], item[1], item[3] or "", item[4] or ""))

    packages = []
    relationships = []
    license_rows = []
    root_ids = []
    for index, (name, package_version, license_expression, source, checksum) in enumerate(
        selected, start=1
    ):
        identifier = _spdx_id(name, index)
        declared = license_expression or "NOASSERTION"
        package: dict[str, Any] = {
            "SPDXID": identifier,
            "name": name,
            "versionInfo": package_version,
            "downloadLocation": source or "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": declared,
            "copyrightText": "NOASSERTION",
        }
        if checksum is not None and SHA256_PATTERN.fullmatch(checksum):
            package["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
        packages.append(package)
        license_rows.append((name, package_version, declared))
        if name in ("devicerail-daemon", "devicerail-bundle-cli"):
            root_ids.append(identifier)
    if len(root_ids) != 2:
        raise ReleaseError("SBOM could not identify both shipped Cargo packages")
    for identifier in sorted(root_ids):
        relationships.append(
            {
                "spdxElementId": "SPDXRef-DOCUMENT",
                "relationshipType": "DESCRIBES",
                "relatedSpdxElement": identifier,
            }
        )

    document_name = f"DeviceRail-{version}-{platform}-{architecture}"
    created = _source_date(epoch)
    namespace_seed = _sha256_bytes(
        _canonical_json(
            {
                "name": document_name,
                "created": created,
                "packages": packages,
                "relationships": relationships,
                "releaseIdentity": release_identity,
            }
        )
    )
    sbom = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": document_name,
        "documentNamespace": f"https://devicerail.dev/spdxdocs/{namespace_seed}",
        "creationInfo": {
            "created": created,
            "creators": ["Tool: DeviceRail-release-packager-1"],
        },
        "packages": packages,
        "relationships": relationships,
    }
    rows = [
        "# Third-party license inventory",
        "",
        "This generated inventory records Cargo metadata only. It is not a substitute",
        "for upstream license text or legal review.",
        "",
        "| Package | Version | Declared license |",
        "| --- | --- | --- |",
    ]
    for name, package_version, declared in license_rows:
        safe = tuple(value.replace("|", "\\|") for value in (name, package_version, declared))
        rows.append(f"| {safe[0]} | {safe[1]} | {safe[2]} |")
    rows.append("")
    return _canonical_json(sbom), ("\n".join(rows)).encode("utf-8")


def _run_checked(arguments: Sequence[str]) -> CommandOutput:
    if not arguments:
        raise ReleaseError("empty verifier command")
    executable = shutil.which(arguments[0])
    if executable is None:
        raise ReleaseError(f"required verification tool is unavailable: {arguments[0]}")
    command = [executable, *arguments[1:]]
    try:
        result = subprocess.run(
            command,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=120,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ReleaseError(f"signature verification failed with {arguments[0]}") from error
    if len(result.stdout) > MAX_COMMAND_OUTPUT_BYTES or len(result.stderr) > MAX_COMMAND_OUTPUT_BYTES:
        raise ReleaseError(f"verification output exceeded its limit: {arguments[0]}")
    return CommandOutput(stdout=result.stdout, stderr=result.stderr)


def _verify_payload_signatures(
    platform: str,
    daemon: Path,
    bundle: Path,
    linux_public_key: Path | None,
    linux_daemon_signature: Path | None,
    linux_bundle_signature: Path | None,
    runner: Runner = _run_checked,
    macos_team_id: str | None = None,
    macos_designated_requirement: str | None = None,
    macos_signing_identity: str | None = None,
    windows_publisher_subject: str | None = None,
    windows_publisher_sha256: str | None = None,
) -> None:
    if platform == "macos":
        _validate_macos_signing_config(
            macos_team_id,
            macos_designated_requirement,
            macos_signing_identity,
        )
        assert macos_team_id is not None
        assert macos_designated_requirement is not None
        assert macos_signing_identity is not None
        for binary in (daemon, bundle):
            runner(
                [
                    "codesign",
                    "--verify",
                    "--strict",
                    "--verbose=2",
                    f"-R={macos_designated_requirement}",
                    str(binary),
                ]
            )
            inspection = runner(
                ["codesign", "-d", "-r-", "--verbose=4", str(binary)]
            )
            if not isinstance(inspection, CommandOutput):
                raise ReleaseError("macOS signature inspection output is unavailable")
            _validate_macos_signature_output(
                inspection,
                macos_team_id,
                macos_signing_identity,
            )
    elif platform == "windows":
        _validate_windows_signing_config(
            windows_publisher_subject,
            windows_publisher_sha256,
        )
        assert windows_publisher_subject is not None
        assert windows_publisher_sha256 is not None
        for binary in (daemon, bundle):
            runner(["signtool", "verify", "/pa", "/all", "/v", str(binary)])
            inspection = runner(
                [
                    "powershell.exe",
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    WINDOWS_SIGNATURE_INSPECTION,
                    str(binary),
                ]
            )
            if not isinstance(inspection, CommandOutput):
                raise ReleaseError("Windows signature inspection output is unavailable")
            _validate_windows_signature_output(
                inspection,
                windows_publisher_subject,
                windows_publisher_sha256,
            )
    elif platform == "linux":
        if None in (
            linux_public_key,
            linux_daemon_signature,
            linux_bundle_signature,
        ):
            raise ReleaseError("signed Linux payloads require a cosign public key and two signatures")
        assert linux_public_key is not None
        assert linux_daemon_signature is not None
        assert linux_bundle_signature is not None
        runner(
            [
                "cosign",
                "verify-blob",
                "--key",
                str(linux_public_key),
                "--signature",
                str(linux_daemon_signature),
                str(daemon),
            ]
        )
        runner(
            [
                "cosign",
                "verify-blob",
                "--key",
                str(linux_public_key),
                "--signature",
                str(linux_bundle_signature),
                str(bundle),
            ]
        )
    else:
        raise ReleaseError("unsupported release platform")


def _validate_macos_signing_config(
    team_id: str | None,
    designated_requirement: str | None,
    signing_identity: str | None,
) -> None:
    if team_id is None or not re.fullmatch(r"[A-Z0-9]{10}", team_id):
        raise ReleaseError("signed macOS payloads require a valid expected Team ID")
    for value, label, maximum in (
        (designated_requirement, "designated requirement", 512),
        (signing_identity, "signing identity", 192),
    ):
        if (
            value is None
            or not value
            or len(value.encode("utf-8")) > maximum
            or not value.isascii()
            or any(ord(character) < 0x20 or ord(character) == 0x7F for character in value)
        ):
            raise ReleaseError(f"signed macOS payloads require a bounded expected {label}")


def _validate_windows_signing_config(
    publisher_subject: str | None,
    publisher_sha256: str | None,
) -> None:
    if (
        publisher_subject is None
        or not publisher_subject
        or publisher_subject != publisher_subject.strip()
        or len(publisher_subject.encode("utf-8")) > 512
        or any(ord(character) < 0x20 or ord(character) == 0x7F for character in publisher_subject)
    ):
        raise ReleaseError(
            "signed Windows payloads require a bounded expected publisher subject"
        )
    if (
        publisher_sha256 is None
        or not CERTIFICATE_SHA256_PATTERN.fullmatch(publisher_sha256)
    ):
        raise ReleaseError(
            "signed Windows payloads require an expected SHA-256 certificate thumbprint"
        )


def _validate_windows_signature_output(
    output: CommandOutput,
    publisher_subject: str,
    publisher_sha256: str,
) -> None:
    try:
        stdout = output.stdout.decode("utf-8", errors="strict")
        stderr = output.stderr.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise ReleaseError("Windows signature inspection was not UTF-8") from error
    if stderr:
        raise ReleaseError("Windows signature inspection wrote unexpected diagnostics")
    lines = stdout.splitlines()
    if lines != [publisher_subject, publisher_sha256.lower()]:
        raise ReleaseError(
            "Windows signature does not match the expected publisher subject and certificate"
        )


def _validate_macos_signature_output(
    output: CommandOutput,
    team_id: str,
    signing_identity: str,
) -> None:
    try:
        text = (output.stdout + output.stderr).decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise ReleaseError("macOS signature inspection was not UTF-8") from error
    lines = [line.strip() for line in text.splitlines()]
    authorities = [line.removeprefix("Authority=") for line in lines if line.startswith("Authority=")]
    if (
        f"TeamIdentifier={team_id}" not in lines
        or signing_identity not in authorities
        or not any(
            line.startswith("CodeDirectory ")
            and "flags=" in line
            and "runtime" in line
            for line in lines
        )
        or not any(
            line.startswith("designated => ") and line != "designated => adhoc"
            for line in lines
        )
        or any(line == "Signature=adhoc" for line in lines)
    ):
        raise ReleaseError(
            "macOS signature lacks the expected Team ID, identity, designated requirement, or hardened runtime"
        )


def _binary_version(binary: Path, expected_name: str, version: str) -> None:
    child_environment = {
        key: value
        for key, value in os.environ.items()
        if key.upper()
        in {
            "PATH",
            "SYSTEMROOT",
            "WINDIR",
            "PATHEXT",
            "TEMP",
            "TMP",
            "TMPDIR",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
        }
    }
    try:
        result = subprocess.run(
            [str(binary), "--version"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
            env=child_environment,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ReleaseError(f"{expected_name} did not report its embedded version") from error
    expected = f"{expected_name} {version}\n".encode("utf-8")
    if result.stdout != expected or result.stderr:
        raise ReleaseError(f"{expected_name} embedded version is inconsistent")


def _verify_binary_target(
    data: bytes, platform: str, architecture: str, label: str
) -> None:
    """Reject a payload whose executable header disagrees with its release label."""

    expected_machine = {
        "linux": {"x86_64": 62, "aarch64": 183},
        "macos": {"x86_64": 0x01000007, "aarch64": 0x0100000C},
        "windows": {"x86_64": 0x8664, "aarch64": 0xAA64},
    }[platform][architecture]
    actual_machine: int | None = None
    if platform == "linux":
        if len(data) >= 20 and data[:4] == b"\x7fELF" and data[4] == 2:
            byte_order = {1: "little", 2: "big"}.get(data[5])
            if byte_order is not None:
                actual_machine = int.from_bytes(data[18:20], byte_order)
    elif platform == "macos":
        byte_order = {
            b"\xcf\xfa\xed\xfe": "little",
            b"\xfe\xed\xfa\xcf": "big",
        }.get(data[:4])
        if len(data) >= 8 and byte_order is not None:
            actual_machine = int.from_bytes(data[4:8], byte_order)
    else:
        if len(data) >= 64 and data[:2] == b"MZ":
            pe_offset = int.from_bytes(data[0x3C:0x40], "little")
            if pe_offset <= len(data) - 6 and data[pe_offset : pe_offset + 4] == b"PE\0\0":
                actual_machine = int.from_bytes(data[pe_offset + 4 : pe_offset + 6], "little")
    if actual_machine != expected_machine:
        raise ReleaseError(
            f"{label} executable target does not match {platform}-{architecture}"
        )


def _host_platform() -> str | None:
    if sys.platform.startswith("linux"):
        return "linux"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform in ("win32", "cygwin"):
        return "windows"
    return None


def _expected_binary_contract(
    payload: Mapping[str, tuple[bytes, int]],
    version: str,
    platform: str,
    architecture: str,
) -> dict[str, Any]:
    suffix = ".exe" if platform == "windows" else ""
    identities = [
        (f"bin/devicerail-bundle{suffix}", "devicerail-bundle"),
        (f"bin/devicerail-daemon{suffix}", "devicerail-daemon"),
    ]
    expected_paths = {path for path, _name in identities}
    actual_paths = {path for path in payload if path.startswith("bin/")}
    if actual_paths != expected_paths:
        raise ReleaseError("release binary inventory is not exactly the expected pair")
    binaries = []
    for path, name in identities:
        entry = payload.get(path)
        if entry is None:
            raise ReleaseError("release binary payload is missing")
        data, mode = entry
        if mode != 0o755:
            raise ReleaseError("release binary payload mode is invalid")
        _verify_binary_target(data, platform, architecture, name)
        binaries.append(
            {
                "path": path,
                "name": name,
                "reportedVersion": version,
                "platform": platform,
                "architecture": architecture,
                "sha256": _sha256_bytes(data),
            }
        )
    return {
        "schemaVersion": 1,
        "contract": "package-time-executed-version-v1",
        "versionCheckArgument": "--version",
        "binaries": binaries,
    }


def _artifact_basename(
    version: str, platform: str, architecture: str, release_status: str
) -> str:
    unsigned = "-UNSIGNED" if release_status == "unsigned-test-only" else ""
    extension = ".tar.gz" if platform == "linux" else ".zip"
    return f"devicerail-{version}-{platform}-{architecture}{unsigned}{extension}"


def _write_zip(path: Path, root: str, files: Mapping[str, tuple[bytes, int]], epoch: int) -> None:
    timestamp = max(epoch, 315532800)
    date_time = dt.datetime.fromtimestamp(timestamp, tz=dt.timezone.utc).timetuple()[:6]
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_STORED, allowZip64=False) as archive:
        for relative, (data, mode) in sorted(files.items()):
            info = zipfile.ZipInfo(f"{root}/{relative}", date_time=date_time)
            info.create_system = 3
            info.compress_type = zipfile.ZIP_STORED
            info.external_attr = (stat.S_IFREG | mode) << 16
            archive.writestr(info, data)


def _write_tar_gz(path: Path, root: str, files: Mapping[str, tuple[bytes, int]], epoch: int) -> None:
    with path.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch, compresslevel=9) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
                for relative, (data, mode) in sorted(files.items()):
                    info = tarfile.TarInfo(f"{root}/{relative}")
                    info.size = len(data)
                    info.mode = mode
                    info.mtime = epoch
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    archive.addfile(info, io.BytesIO(data))


def _publish_file(source: Path, destination: Path) -> None:
    try:
        descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
        with os.fdopen(descriptor, "wb") as output, source.open("rb") as input_file:
            shutil.copyfileobj(input_file, output, 1024 * 1024)
            output.flush()
            os.fsync(output.fileno())
    except FileExistsError as error:
        raise ReleaseError(f"release output already exists: {destination.name}") from error
    except OSError as error:
        raise ReleaseError(f"could not publish {destination.name}") from error


def _publish_bytes(data: bytes, destination: Path) -> None:
    try:
        with destination.open("xb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
    except FileExistsError as error:
        raise ReleaseError(f"release output already exists: {destination.name}") from error
    except OSError as error:
        raise ReleaseError(f"could not publish {destination.name}") from error


def package_release(args: argparse.Namespace, runner: Runner = _run_checked) -> dict[str, Any]:
    repo_root = args.repo_root.resolve()
    output_dir = args.output_dir.resolve()
    if args.platform not in PLATFORMS or args.architecture not in ARCHITECTURES:
        raise ReleaseError("unsupported platform or architecture")
    _validate_source_date_epoch(args.source_date_epoch, args.platform)
    try:
        output_metadata = output_dir.lstat()
    except OSError as error:
        raise ReleaseError("output directory must already exist") from error
    if stat.S_ISLNK(output_metadata.st_mode) or not stat.S_ISDIR(output_metadata.st_mode):
        raise ReleaseError("output directory must be a real directory")

    test_metadata_path = getattr(args, "test_cargo_metadata", None)
    metadata = _cargo_metadata(repo_root, test_metadata_path)
    version = _workspace_version(repo_root, metadata)
    executable_suffix = ".exe" if args.platform == "windows" else ""
    daemon_data = _read_regular_file(args.daemon, MAX_BINARY_BYTES, "daemon binary")
    bundle_data = _read_regular_file(args.bundle, MAX_BINARY_BYTES, "Bundle CLI binary")
    _verify_binary_target(daemon_data, args.platform, args.architecture, "daemon")
    _verify_binary_target(bundle_data, args.platform, args.architecture, "Bundle CLI")

    signed = args.release_status == "signed"
    if args.release_status not in ("signed", "unsigned-test-only"):
        raise ReleaseError("release status must be signed or unsigned-test-only")
    source_parameters, source_material = _source_provenance(
        repo_root,
        signed=signed,
        source_uri=getattr(args, "source_uri", None),
        test_metadata_path=test_metadata_path,
    )
    if signed:
        _require_signed_source_claim(source_parameters, source_material)
    signing_inputs = (
        args.linux_public_key,
        args.linux_daemon_signature,
        args.linux_bundle_signature,
        args.artifact_public_key,
    )
    if not signed and any(value is not None for value in signing_inputs):
        raise ReleaseError("unsigned artifacts must not accept signature inputs")
    if signed and args.artifact_public_key is None:
        raise ReleaseError("signed artifacts require an explicit cosign archive public key")
    linux_inputs = (
        args.linux_public_key,
        args.linux_daemon_signature,
        args.linux_bundle_signature,
    )
    if signed and args.platform == "linux" and any(value is None for value in linux_inputs):
        raise ReleaseError("signed Linux artifacts require a public key and two payload signatures")
    if signed and args.platform != "linux" and any(value is not None for value in linux_inputs):
        raise ReleaseError("Linux payload signature inputs are invalid for this platform")
    macos_team_id = getattr(args, "macos_team_id", None)
    macos_designated_requirement = getattr(args, "macos_designated_requirement", None)
    macos_signing_identity = getattr(args, "macos_signing_identity", None)
    macos_inputs = (
        macos_team_id,
        macos_designated_requirement,
        macos_signing_identity,
    )
    if signed and args.platform == "macos":
        _validate_macos_signing_config(*macos_inputs)
    elif any(value is not None for value in macos_inputs):
        raise ReleaseError("macOS signing inputs are invalid for this platform or release status")
    windows_publisher_subject = getattr(args, "windows_publisher_subject", None)
    windows_publisher_sha256 = getattr(args, "windows_publisher_sha256", None)
    windows_inputs = (windows_publisher_subject, windows_publisher_sha256)
    if signed and args.platform == "windows":
        _validate_windows_signing_config(*windows_inputs)
        assert windows_publisher_sha256 is not None
        windows_publisher_sha256 = windows_publisher_sha256.lower()
    elif any(value is not None for value in windows_inputs):
        raise ReleaseError("Windows signing inputs are invalid for this platform or release status")

    linux_public_key_data = None
    daemon_signature_data = None
    bundle_signature_data = None
    artifact_public_key_data = None
    if signed:
        assert args.artifact_public_key is not None
        artifact_public_key_data = _read_regular_file(
            args.artifact_public_key, 64 * 1024, "archive cosign public key"
        )
        if args.platform == "linux":
            assert args.linux_public_key is not None
            assert args.linux_daemon_signature is not None
            assert args.linux_bundle_signature is not None
            linux_public_key_data = _read_regular_file(
                args.linux_public_key, 64 * 1024, "Linux cosign public key"
            )
            daemon_signature_data = _read_regular_file(
                args.linux_daemon_signature, 64 * 1024, "daemon cosign signature"
            )
            bundle_signature_data = _read_regular_file(
                args.linux_bundle_signature, 64 * 1024, "Bundle CLI cosign signature"
            )

    exact_inputs: dict[str, tuple[bytes, int]] = {
        f"devicerail-daemon{executable_suffix}": (daemon_data, 0o755),
        f"devicerail-bundle{executable_suffix}": (bundle_data, 0o755),
    }
    if args.platform == "linux" and signed:
        assert linux_public_key_data is not None
        assert daemon_signature_data is not None
        assert bundle_signature_data is not None
        exact_inputs["signatures/cosign.pub"] = (linux_public_key_data, 0o644)
        exact_inputs["signatures/devicerail-daemon.sig"] = (daemon_signature_data, 0o644)
        exact_inputs["signatures/devicerail-bundle.sig"] = (bundle_signature_data, 0o644)
    with tempfile.TemporaryDirectory(prefix="devicerail-package-input-") as temporary:
        materialized = _materialize_payload(Path(temporary), exact_inputs)
        exact_daemon = materialized[f"devicerail-daemon{executable_suffix}"]
        exact_bundle = materialized[f"devicerail-bundle{executable_suffix}"]
        if signed:
            _verify_payload_signatures(
                args.platform,
                exact_daemon,
                exact_bundle,
                materialized.get("signatures/cosign.pub"),
                materialized.get("signatures/devicerail-daemon.sig"),
                materialized.get("signatures/devicerail-bundle.sig"),
                runner,
                macos_team_id=macos_team_id,
                macos_designated_requirement=macos_designated_requirement,
                macos_signing_identity=macos_signing_identity,
                windows_publisher_subject=windows_publisher_subject,
                windows_publisher_sha256=windows_publisher_sha256,
            )
        _binary_version(exact_daemon, "devicerail-daemon", version)
        _binary_version(exact_bundle, "devicerail-bundle", version)

    sbom, license_inventory = _build_sbom(
        metadata,
        version,
        args.platform,
        args.architecture,
        args.source_date_epoch,
        {
            "sourceParameters": source_parameters,
            "sourceMaterial": source_material,
            "binaries": [
                {
                    "name": "devicerail-bundle",
                    "sha256": _sha256_bytes(bundle_data),
                },
                {
                    "name": "devicerail-daemon",
                    "sha256": _sha256_bytes(daemon_data),
                },
            ],
        },
    )
    assets = repo_root / "packaging" / "assets"
    files: dict[str, tuple[bytes, int]] = {
        f"bin/devicerail-daemon{executable_suffix}": (daemon_data, 0o755),
        f"bin/devicerail-bundle{executable_suffix}": (bundle_data, 0o755),
        "config/devicerail.env.example": (
            _read_regular_file(
                assets / "devicerail.env.example", MAX_TEXT_BYTES, "configuration example"
            ),
            0o644,
        ),
        "DISTRIBUTION-README.txt": (
            _read_regular_file(
                assets / "DISTRIBUTION-README.txt", MAX_TEXT_BYTES, "distribution README"
            ),
            0o644,
        ),
        "LICENSE": (
            _read_regular_file(
                repo_root / "LICENSE", MAX_TEXT_BYTES, "DeviceRail license"
            ),
            0o644,
        ),
        "NOTICE": (
            _read_regular_file(repo_root / "NOTICE", MAX_TEXT_BYTES, "DeviceRail notice"),
            0o644,
        ),
        "LICENSES/THIRD-PARTY-LICENSES.md": (license_inventory, 0o644),
        "SBOM.spdx.json": (sbom, 0o644),
    }
    if args.platform == "windows":
        files["install.ps1"] = (
            _read_regular_file(assets / "install.ps1", MAX_TEXT_BYTES, "Windows installer"),
            0o644,
        )
    else:
        files["install.sh"] = (
            _read_regular_file(assets / "install.sh", MAX_TEXT_BYTES, "Unix installer"),
            0o755,
        )

    payload_scheme = {
        "linux": "cosign-key",
        "macos": "apple-codesign",
        "windows": "authenticode",
    }[args.platform]
    binary_paths = sorted(
        [f"bin/devicerail-daemon{executable_suffix}", f"bin/devicerail-bundle{executable_suffix}"]
    )
    files["BINARY-METADATA.json"] = (
        _canonical_json(
            _expected_binary_contract(
                files,
                version,
                args.platform,
                args.architecture,
            )
        ),
        0o644,
    )
    signing: dict[str, Any] = {
        "status": args.release_status,
        "payloadScheme": payload_scheme if signed else "none",
        "payloadSignedFiles": binary_paths if signed else [],
        "archiveScheme": "cosign-key" if signed else "none",
        "archiveSignatureRequired": signed,
    }
    if signed:
        assert artifact_public_key_data is not None
        signing["archivePublicKeySha256"] = _sha256_bytes(artifact_public_key_data)
    if args.platform == "macos" and signed:
        signing["macosTeamId"] = macos_team_id
        signing["macosDesignatedRequirement"] = macos_designated_requirement
        signing["macosSigningIdentity"] = macos_signing_identity
    if args.platform == "windows" and signed:
        signing["windowsPublisherSubject"] = windows_publisher_subject
        signing["windowsPublisherSha256"] = windows_publisher_sha256
    if args.platform == "linux" and signed:
        assert linux_public_key_data is not None
        assert daemon_signature_data is not None
        assert bundle_signature_data is not None
        files["signatures/cosign.pub"] = (linux_public_key_data, 0o644)
        files["signatures/devicerail-daemon.sig"] = (daemon_signature_data, 0o644)
        files["signatures/devicerail-bundle.sig"] = (bundle_signature_data, 0o644)
        signing["payloadPublicKeySha256"] = _sha256_bytes(linux_public_key_data)
    files["SIGNING.json"] = (_canonical_json(signing), 0o644)

    binary_subjects = [
        {
            "name": path,
            "digest": {"sha256": _sha256_bytes(files[path][0])},
        }
        for path in binary_paths
    ]
    build_provenance = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": binary_subjects,
        "predicateType": PROVENANCE_PREDICATE_TYPE,
        "predicate": {
            "buildDefinition": {
                "buildType": "https://devicerail.dev/build-types/cargo-release-v1",
                "externalParameters": {
                    "version": version,
                    "platform": args.platform,
                    "architecture": args.architecture,
                    "sourceDateEpoch": args.source_date_epoch,
                    **source_parameters,
                },
                "internalParameters": {},
                "resolvedDependencies": [source_material],
            },
            "runDetails": {
                "builder": {"id": PROVENANCE_BUILDER_ID},
                "metadata": {
                    "invocationId": "",
                    "startedOn": _source_date(args.source_date_epoch),
                    "finishedOn": _source_date(args.source_date_epoch),
                },
            },
        },
    }
    files["BUILD-PROVENANCE.intoto.json"] = (_canonical_json(build_provenance), 0o644)

    file_manifest = [
        {
            "path": path,
            "sha256": _sha256_bytes(data),
            "size": len(data),
            "mode": f"{mode:04o}",
        }
        for path, (data, mode) in sorted(files.items())
    ]
    warning = (
        None
        if signed
        else "UNSIGNED TEST ARTIFACT: integrity-checked but not authenticated for production release"
    )
    manifest = {
        "schemaVersion": SCHEMA_VERSION,
        "name": "DeviceRail",
        "version": version,
        "platform": args.platform,
        "architecture": args.architecture,
        "distribution": "portable-installer-archive",
        "releaseStatus": args.release_status,
        "sourceDateEpoch": args.source_date_epoch,
        "signing": signing,
        "warning": warning,
        "files": file_manifest,
    }
    files["release-manifest.json"] = (_canonical_json(manifest), 0o644)

    basename = _artifact_basename(version, args.platform, args.architecture, args.release_status)
    artifact = output_dir / basename
    checksum_path = output_dir / f"{basename}.sha256"
    provenance_path = output_dir / f"{basename}.provenance.json"
    public_key_path = output_dir / f"{basename}.cosign.pub"
    root = basename.removesuffix(".tar.gz").removesuffix(".zip")
    output_paths = [artifact, checksum_path, provenance_path]
    if signed:
        output_paths.append(public_key_path)
    if any(path.exists() for path in output_paths):
        raise ReleaseError("one or more release outputs already exist")

    published: list[Path] = []
    try:
        with tempfile.TemporaryDirectory(prefix=".devicerail-package-", dir=output_dir) as temp:
            temporary_artifact = Path(temp) / basename
            if args.platform == "linux":
                _write_tar_gz(temporary_artifact, root, files, args.source_date_epoch)
            else:
                _write_zip(temporary_artifact, root, files, args.source_date_epoch)
            digest, size = _sha256_file(temporary_artifact)
            _publish_file(temporary_artifact, artifact)
            published.append(artifact)

        checksum = f"{digest}  {basename}\n".encode("ascii")
        _publish_bytes(checksum, checksum_path)
        published.append(checksum_path)
        outer_provenance = {
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{"name": basename, "digest": {"sha256": digest}}],
            "predicateType": PROVENANCE_PREDICATE_TYPE,
            "predicate": {
                "buildDefinition": {
                    "buildType": "https://devicerail.dev/build-types/portable-archive-v1",
                    "externalParameters": {
                        "version": version,
                        "platform": args.platform,
                        "architecture": args.architecture,
                        "releaseStatus": args.release_status,
                        "sourceDateEpoch": args.source_date_epoch,
                        **source_parameters,
                    },
                    "internalParameters": {},
                    "resolvedDependencies": [
                        source_material,
                        *(
                            {
                                "uri": f"file:{subject['name']}",
                                "digest": subject["digest"],
                            }
                            for subject in binary_subjects
                        ),
                    ],
                },
                "runDetails": {
                    "builder": {"id": PROVENANCE_BUILDER_ID},
                    "metadata": {
                        "invocationId": "",
                        "startedOn": _source_date(args.source_date_epoch),
                        "finishedOn": _source_date(args.source_date_epoch),
                    },
                },
            },
        }
        _publish_bytes(_canonical_json(outer_provenance), provenance_path)
        published.append(provenance_path)
        if signed:
            assert artifact_public_key_data is not None
            _publish_bytes(artifact_public_key_data, public_key_path)
            published.append(public_key_path)
    except Exception:
        for path in reversed(published):
            try:
                path.unlink()
            except OSError:
                pass
        raise

    return {
        "ok": True,
        "artifact": str(artifact),
        "sha256": digest,
        "size": size,
        "releaseStatus": args.release_status,
        "checksum": str(checksum_path),
        "provenance": str(provenance_path),
        "archiveSignature": str(output_dir / f"{basename}.sig") if signed else None,
        "archivePublicKey": str(public_key_path) if signed else None,
    }


def _zip_extra_contains_zip64(extra: bytes) -> bool:
    offset = 0
    while offset < len(extra):
        if len(extra) - offset < 4:
            raise ReleaseError("ZIP central-directory extra data is malformed")
        header_id, size = struct.unpack_from("<HH", extra, offset)
        offset += 4
        if size > len(extra) - offset:
            raise ReleaseError("ZIP central-directory extra data is malformed")
        if header_id == 0x0001:
            return True
        offset += size
    return False


def _preflight_zip(path: Path) -> int:
    """Bound the central directory before ZipFile allocates one object per entry."""

    try:
        with path.open("rb") as source:
            size = os.fstat(source.fileno()).st_size
            if size < 22 or size > MAX_ARCHIVE_BYTES:
                raise ReleaseError("ZIP archive size is outside its limits")
            source.seek(size - 22)
            eocd = source.read(22)
            if len(eocd) != 22:
                raise ReleaseError("ZIP end-of-central-directory is truncated")
            (
                signature,
                disk_number,
                central_disk,
                disk_entries,
                total_entries,
                central_size,
                central_offset,
                comment_length,
            ) = struct.unpack("<4s4H2LH", eocd)
            if signature != b"PK\x05\x06":
                raise ReleaseError("ZIP end-of-central-directory is missing")
            if comment_length != 0:
                raise ReleaseError("ZIP archive comment is not allowed")
            if disk_number != 0 or central_disk != 0 or disk_entries != total_entries:
                raise ReleaseError("multi-disk ZIP archives are not supported")
            if (
                total_entries == 0xFFFF
                or central_size == 0xFFFFFFFF
                or central_offset == 0xFFFFFFFF
            ):
                raise ReleaseError("ZIP64 archives are not supported")
            if total_entries > MAX_ARCHIVE_ENTRIES:
                raise ReleaseError("ZIP archive entry count exceeds its limit")
            if central_size > MAX_ZIP_CENTRAL_DIRECTORY_BYTES:
                raise ReleaseError("ZIP central directory exceeds its limit")
            if central_offset + central_size != size - 22:
                raise ReleaseError("ZIP central-directory bounds are inconsistent")
            source.seek(central_offset)
            central = source.read(central_size)
    except ReleaseError:
        raise
    except (OSError, struct.error) as error:
        raise ReleaseError("ZIP archive preflight failed") from error
    if len(central) != central_size:
        raise ReleaseError("ZIP central directory is truncated")

    offset = 0
    parsed_entries = 0
    while offset < len(central):
        if len(central) - offset < 46 or central[offset : offset + 4] != b"PK\x01\x02":
            raise ReleaseError("ZIP central-directory entry is malformed")
        (
            compressed_size,
            uncompressed_size,
            filename_length,
            extra_length,
            member_comment_length,
            disk_start,
            local_header_offset,
        ) = struct.unpack_from("<LLHHHHxxxxxxL", central, offset + 20)
        entry_size = 46 + filename_length + extra_length + member_comment_length
        if entry_size > len(central) - offset:
            raise ReleaseError("ZIP central-directory entry is truncated")
        extra_start = offset + 46 + filename_length
        extra = central[extra_start : extra_start + extra_length]
        if (
            compressed_size == 0xFFFFFFFF
            or uncompressed_size == 0xFFFFFFFF
            or disk_start == 0xFFFF
            or local_header_offset == 0xFFFFFFFF
            or _zip_extra_contains_zip64(extra)
        ):
            raise ReleaseError("ZIP64 archives are not supported")
        parsed_entries += 1
        if parsed_entries > MAX_ARCHIVE_ENTRIES:
            raise ReleaseError("ZIP archive entry count exceeds its limit")
        offset += entry_size
    if parsed_entries != total_entries:
        raise ReleaseError("ZIP central-directory entry count is inconsistent")
    return total_entries


def _read_archive(path: Path) -> dict[str, tuple[bytes, int]]:
    result: dict[str, tuple[bytes, int]] = {}
    folded: set[str] = set()
    total = 0

    def add(name: str, data: bytes, mode: int) -> None:
        nonlocal total
        _safe_relative_path(name)
        folded_name = name.casefold()
        if name in result or folded_name in folded:
            raise ReleaseError("archive contains duplicate or case-colliding members")
        if mode not in (0o644, 0o755):
            raise ReleaseError("archive member has a non-canonical mode")
        total += len(data)
        if len(result) >= MAX_ARCHIVE_ENTRIES or total > MAX_ARCHIVE_BYTES:
            raise ReleaseError("archive expands beyond its limits")
        folded.add(folded_name)
        result[name] = (data, mode)

    if path.name.endswith(".zip"):
        expected_entries = _preflight_zip(path)
        try:
            with zipfile.ZipFile(path, "r", allowZip64=False) as archive:
                if archive.comment:
                    raise ReleaseError("ZIP archive comment is not allowed")
                if len(archive.filelist) != expected_entries:
                    raise ReleaseError("ZIP entry count changed after preflight")
                for info in archive.filelist:
                    if info.is_dir():
                        raise ReleaseError("explicit archive directory entries are not allowed")
                    if info.flag_bits & 0x1 or info.compress_type != zipfile.ZIP_STORED:
                        raise ReleaseError("ZIP member is encrypted or non-canonical")
                    if info.create_system != 3:
                        raise ReleaseError("ZIP member lacks portable Unix mode metadata")
                    raw_mode = info.external_attr >> 16
                    if stat.S_IFMT(raw_mode) != stat.S_IFREG:
                        raise ReleaseError("ZIP links and special members are not allowed")
                    if (
                        info.compress_size != info.file_size
                        or info.file_size > MAX_ARCHIVE_BYTES - total
                    ):
                        raise ReleaseError(
                            "ZIP member size is non-canonical or exceeds the limit"
                        )
                    data = archive.read(info)
                    if len(data) != info.file_size:
                        raise ReleaseError("ZIP member size is inconsistent")
                    add(info.filename, data, stat.S_IMODE(raw_mode))
        except ReleaseError:
            raise
        except (OSError, zipfile.BadZipFile, RuntimeError) as error:
            raise ReleaseError("ZIP archive is malformed") from error
    elif path.name.endswith(".tar.gz"):
        try:
            before = path.lstat()
            if (
                stat.S_ISLNK(before.st_mode)
                or not stat.S_ISREG(before.st_mode)
                or before.st_size > MAX_ARCHIVE_BYTES
            ):
                raise ReleaseError("tar archive must be a bounded regular file")
            flags = os.O_RDONLY
            if hasattr(os, "O_BINARY"):
                flags |= os.O_BINARY
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            descriptor = os.open(path, flags)
            with os.fdopen(descriptor, "rb") as raw:
                opened = os.fstat(raw.fileno())
                if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                    raise ReleaseError("tar archive changed while it was opened")
                with gzip.GzipFile(fileobj=raw, mode="rb") as compressed:
                    bounded = _BoundedReadStream(
                        compressed,
                        MAX_TAR_STREAM_BYTES,
                        "tar archive",
                    )
                    with tarfile.open(fileobj=bounded, mode="r|") as archive:
                        member_count = 0
                        for member in archive:
                            member_count += 1
                            if member_count > MAX_ARCHIVE_ENTRIES:
                                raise ReleaseError("tar archive entry count exceeds its limit")
                            if not member.isreg() or member.issparse():
                                raise ReleaseError(
                                    "tar links, directories, and special members are not allowed"
                                )
                            if member.size > MAX_ARCHIVE_BYTES - total:
                                raise ReleaseError("tar member exceeds the size limit")
                            source = archive.extractfile(member)
                            if source is None:
                                raise ReleaseError("tar member could not be read")
                            data = source.read(member.size + 1)
                            if len(data) != member.size:
                                raise ReleaseError("tar member size is inconsistent")
                            add(member.name, data, member.mode)
                    while bounded.read(64 * 1024):
                        pass
                after = os.fstat(raw.fileno())
                if (
                    after.st_size != before.st_size
                    or after.st_mtime_ns != before.st_mtime_ns
                ):
                    raise ReleaseError("tar archive changed while it was read")
        except ReleaseError:
            raise
        except (OSError, tarfile.TarError) as error:
            raise ReleaseError("tar archive is malformed") from error
    else:
        raise ReleaseError("release archive must be .zip or .tar.gz")
    if not result:
        raise ReleaseError("release archive is empty")
    return result


def _manifest_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ReleaseError(f"manifest {label} must be a non-empty string")
    return value


def _validate_manifest(
    artifact: Path, entries: Mapping[str, tuple[bytes, int]]
) -> tuple[str, Mapping[str, Any], Mapping[str, tuple[bytes, int]]]:
    roots = {PurePosixPath(name).parts[0] for name in entries}
    if len(roots) != 1:
        raise ReleaseError("archive must contain exactly one top-level root")
    root = next(iter(roots))
    manifest_entry = f"{root}/release-manifest.json"
    if manifest_entry not in entries:
        raise ReleaseError("release manifest is missing")
    manifest = _load_json_bytes(entries[manifest_entry][0], "release manifest")
    if not isinstance(manifest, dict):
        raise ReleaseError("release manifest must be an object")
    expected_keys = {
        "schemaVersion",
        "name",
        "version",
        "platform",
        "architecture",
        "distribution",
        "releaseStatus",
        "sourceDateEpoch",
        "signing",
        "warning",
        "files",
    }
    if (
        set(manifest) != expected_keys
        or isinstance(manifest.get("schemaVersion"), bool)
        or not isinstance(manifest.get("schemaVersion"), int)
        or manifest.get("schemaVersion") != SCHEMA_VERSION
    ):
        raise ReleaseError("release manifest schema is not supported")
    if manifest.get("name") != "DeviceRail" or manifest.get("distribution") != "portable-installer-archive":
        raise ReleaseError("release manifest identity is invalid")
    version = _manifest_string(manifest.get("version"), "version")
    platform = _manifest_string(manifest.get("platform"), "platform")
    architecture = _manifest_string(manifest.get("architecture"), "architecture")
    status_value = _manifest_string(manifest.get("releaseStatus"), "releaseStatus")
    if not VERSION_PATTERN.fullmatch(version) or platform not in PLATFORMS or architecture not in ARCHITECTURES:
        raise ReleaseError("release manifest target or version is invalid")
    if status_value not in ("signed", "unsigned-test-only"):
        raise ReleaseError("release manifest status is invalid")
    expected_name = _artifact_basename(version, platform, architecture, status_value)
    if artifact.name != expected_name:
        raise ReleaseError("archive filename and release manifest differ")
    expected_root = expected_name.removesuffix(".tar.gz").removesuffix(".zip")
    if root != expected_root:
        raise ReleaseError("archive root and release manifest differ")
    if (
        isinstance(manifest.get("sourceDateEpoch"), bool)
        or not isinstance(manifest.get("sourceDateEpoch"), int)
        or manifest["sourceDateEpoch"] < 0
    ):
        raise ReleaseError("release manifest SOURCE_DATE_EPOCH is invalid")
    _validate_source_date_epoch(manifest["sourceDateEpoch"], platform)
    if status_value == "unsigned-test-only":
        if not isinstance(manifest.get("warning"), str) or "UNSIGNED" not in manifest["warning"]:
            raise ReleaseError("unsigned release manifest lacks its warning")
    elif manifest.get("warning") is not None:
        raise ReleaseError("signed release manifest must not carry an unsigned warning")

    listed = manifest.get("files")
    if not isinstance(listed, list) or not listed:
        raise ReleaseError("release manifest file inventory is empty")
    payload: dict[str, tuple[bytes, int]] = {}
    previous = ""
    for item in listed:
        if not isinstance(item, dict) or set(item) != {"path", "sha256", "size", "mode"}:
            raise ReleaseError("release manifest file entry is malformed")
        relative = _manifest_string(item.get("path"), "file path")
        _safe_relative_path(relative)
        if relative <= previous:
            raise ReleaseError("release manifest file inventory is not uniquely sorted")
        previous = relative
        digest = item.get("sha256")
        size = item.get("size")
        mode_text = item.get("mode")
        if not isinstance(digest, str) or not SHA256_PATTERN.fullmatch(digest):
            raise ReleaseError("release manifest file checksum is invalid")
        if (
            isinstance(size, bool)
            or not isinstance(size, int)
            or size < 0
            or size > MAX_ARCHIVE_BYTES
        ):
            raise ReleaseError("release manifest file size is invalid")
        if mode_text not in ("0644", "0755"):
            raise ReleaseError("release manifest file mode is invalid")
        archive_name = f"{root}/{relative}"
        actual = entries.get(archive_name)
        if actual is None:
            raise ReleaseError("release manifest references a missing file")
        data, mode = actual
        if len(data) != size or _sha256_bytes(data) != digest or mode != int(mode_text, 8):
            raise ReleaseError("release payload does not match its manifest")
        payload[relative] = actual
    expected_entries = {manifest_entry, *(f"{root}/{name}" for name in payload)}
    if set(entries) != expected_entries:
        raise ReleaseError("archive contains unlisted files")

    signing = manifest.get("signing")
    signing_entry = payload.get("SIGNING.json")
    if not isinstance(signing, dict) or signing_entry is None:
        raise ReleaseError("release signing declaration is missing")
    if _canonical_json(_load_json_bytes(signing_entry[0], "SIGNING.json")) != _canonical_json(
        signing
    ):
        raise ReleaseError("release signing declarations differ")
    return root, manifest, payload


def _validated_source_claim(
    parameters: Any,
    resolved_dependencies: Any,
) -> tuple[dict[str, Any], dict[str, Any]]:
    if not isinstance(parameters, dict) or not isinstance(resolved_dependencies, list):
        raise ReleaseError("build provenance source claim is malformed")
    source_keys = {
        "buildMode",
        "cargoLocked",
        "sourceState",
        "sourceMaterialComplete",
    }
    mode = parameters.get("buildMode")
    if mode == "test-fixture":
        if (
            parameters.get("cargoLocked") is not False
            or parameters.get("sourceState") != "test-fixture"
            or parameters.get("sourceMaterialComplete") is not False
            or "workspaceStatusSha256" in parameters
        ):
            raise ReleaseError("test-fixture provenance overstates its source completeness")
    elif mode == "production-git":
        state = parameters.get("sourceState")
        if parameters.get("cargoLocked") is not True or state not in (
            "clean",
            "dirty-uncommitted",
        ):
            raise ReleaseError("production provenance source state is invalid")
        if state == "clean":
            if (
                parameters.get("sourceMaterialComplete") is not True
                or "workspaceStatusSha256" in parameters
            ):
                raise ReleaseError("clean source provenance is inconsistent")
        else:
            workspace_digest = parameters.get("workspaceStatusSha256")
            if (
                parameters.get("sourceMaterialComplete") is not False
                or not isinstance(workspace_digest, str)
                or not SHA256_PATTERN.fullmatch(workspace_digest)
            ):
                raise ReleaseError("dirty source provenance is not explicitly incomplete")
            source_keys.add("workspaceStatusSha256")
    else:
        raise ReleaseError("build provenance mode is unsupported")

    if not source_keys.issubset(parameters):
        raise ReleaseError("build provenance source parameters are incomplete")

    if len(resolved_dependencies) != 1:
        raise ReleaseError("build provenance must identify exactly one source material")
    material = resolved_dependencies[0]
    if not isinstance(material, dict) or set(material) != {"uri", "digest"}:
        raise ReleaseError("build provenance source material is malformed")
    uri = material.get("uri")
    digest = material.get("digest")
    if not isinstance(uri, str) or not isinstance(digest, dict) or len(digest) != 1:
        raise ReleaseError("build provenance source material is malformed")
    if mode == "test-fixture":
        if uri != "test-fixture:cargo-metadata" or set(digest) != {"sha256"}:
            raise ReleaseError("test-fixture source material is invalid")
        value = digest.get("sha256")
        if not isinstance(value, str) or not SHA256_PATTERN.fullmatch(value):
            raise ReleaseError("test-fixture source digest is invalid")
    else:
        if uri != "git+file://workspace":
            _validate_source_uri(uri, signed=False)
        algorithm, value = next(iter(digest.items()))
        expected_length = {"sha1": 40, "sha256": 64}.get(algorithm)
        if (
            expected_length is None
            or not isinstance(value, str)
            or len(value) != expected_length
            or not re.fullmatch(r"[0-9a-f]+", value)
        ):
            raise ReleaseError("git source material digest is invalid")
    return ({key: parameters[key] for key in source_keys}, material)


def _validated_provenance_definition(
    provenance: Any,
    *,
    expected_build_type: str,
    source_date_epoch: int,
    label: str,
) -> Mapping[str, Any]:
    if (
        not isinstance(provenance, dict)
        or set(provenance) != {"_type", "subject", "predicateType", "predicate"}
        or provenance.get("_type") != "https://in-toto.io/Statement/v1"
        or provenance.get("predicateType") != PROVENANCE_PREDICATE_TYPE
    ):
        raise ReleaseError(f"{label} statement shape or type is invalid")
    predicate = provenance.get("predicate")
    if (
        not isinstance(predicate, dict)
        or set(predicate) != {"buildDefinition", "runDetails"}
    ):
        raise ReleaseError(f"{label} predicate is malformed")
    build_definition = predicate.get("buildDefinition")
    if (
        not isinstance(build_definition, dict)
        or set(build_definition)
        != {
            "buildType",
            "externalParameters",
            "internalParameters",
            "resolvedDependencies",
        }
        or build_definition.get("buildType") != expected_build_type
        or build_definition.get("internalParameters") != {}
    ):
        raise ReleaseError(f"{label} build definition is malformed")
    expected_date = _source_date(source_date_epoch)
    run_details = predicate.get("runDetails")
    if (
        not isinstance(run_details, dict)
        or set(run_details) != {"builder", "metadata"}
        or run_details.get("builder") != {"id": PROVENANCE_BUILDER_ID}
        or run_details.get("metadata")
        != {
            "invocationId": "",
            "startedOn": expected_date,
            "finishedOn": expected_date,
        }
    ):
        raise ReleaseError(f"{label} run details are malformed")
    return build_definition


def _validate_embedded_metadata(
    manifest: Mapping[str, Any], payload: Mapping[str, tuple[bytes, int]]
) -> tuple[dict[str, Any], dict[str, Any]]:
    version = manifest["version"]
    platform = manifest["platform"]
    architecture = manifest["architecture"]
    binary_metadata_entry = payload.get("BINARY-METADATA.json")
    sbom_entry = payload.get("SBOM.spdx.json")
    provenance_entry = payload.get("BUILD-PROVENANCE.intoto.json")
    if binary_metadata_entry is None or sbom_entry is None or provenance_entry is None:
        raise ReleaseError("binary metadata, SBOM, or build provenance is missing")
    binary_metadata = _load_json_bytes(binary_metadata_entry[0], "binary metadata")
    expected_binary_metadata = _expected_binary_contract(
        payload,
        version,
        platform,
        architecture,
    )
    if _canonical_json(binary_metadata) != _canonical_json(expected_binary_metadata):
        raise ReleaseError("binary version/target contract differs from the payload")
    sbom = _load_json_bytes(sbom_entry[0], "SPDX SBOM")
    if not isinstance(sbom, dict) or sbom.get("spdxVersion") != "SPDX-2.3":
        raise ReleaseError("SPDX SBOM version is invalid")
    if sbom.get("name") != f"DeviceRail-{version}-{platform}-{architecture}":
        raise ReleaseError("SPDX SBOM identity differs from the manifest")
    namespace = sbom.get("documentNamespace")
    if not isinstance(namespace, str) or not re.fullmatch(
        r"https://devicerail\.dev/spdxdocs/[0-9a-f]{64}",
        namespace,
    ):
        raise ReleaseError("SPDX SBOM document namespace is invalid")
    packages = sbom.get("packages")
    if not isinstance(packages, list):
        raise ReleaseError("SPDX SBOM packages are missing")
    shipped = {
        item.get("name"): item.get("versionInfo")
        for item in packages
        if isinstance(item, dict)
        and item.get("name") in ("devicerail-daemon", "devicerail-bundle-cli")
    }
    if shipped != {"devicerail-daemon": version, "devicerail-bundle-cli": version}:
        raise ReleaseError("SPDX SBOM shipped package versions differ")

    provenance = _load_json_bytes(provenance_entry[0], "build provenance")
    build_definition = _validated_provenance_definition(
        provenance,
        expected_build_type="https://devicerail.dev/build-types/cargo-release-v1",
        source_date_epoch=manifest["sourceDateEpoch"],
        label="build provenance",
    )
    subjects = provenance.get("subject")
    if not isinstance(subjects, list):
        raise ReleaseError("build provenance subjects are missing")
    expected_binary_names = sorted(
        name for name in payload if name.startswith("bin/devicerail-")
    )
    actual_subjects = []
    for subject in subjects:
        if not isinstance(subject, dict) or set(subject) != {"name", "digest"}:
            raise ReleaseError("build provenance subject is malformed")
        name = subject.get("name")
        digest = subject.get("digest")
        if not isinstance(name, str) or not isinstance(digest, dict):
            raise ReleaseError("build provenance subject is malformed")
        entry = payload.get(name)
        if entry is None or digest != {"sha256": _sha256_bytes(entry[0])}:
            raise ReleaseError("build provenance subject checksum differs")
        actual_subjects.append(name)
    if sorted(actual_subjects) != expected_binary_names:
        raise ReleaseError("build provenance does not describe exactly the shipped binaries")

    parameters = build_definition["externalParameters"]
    resolved_dependencies = build_definition["resolvedDependencies"]
    source_parameters, source_material = _validated_source_claim(
        parameters,
        resolved_dependencies,
    )
    expected_parameters = {
        "version": version,
        "platform": platform,
        "architecture": architecture,
        "sourceDateEpoch": manifest["sourceDateEpoch"],
        **source_parameters,
    }
    if parameters != expected_parameters:
        raise ReleaseError("build provenance parameters differ from the manifest")
    return source_parameters, source_material


def _validate_checksum_sidecar(artifact: Path, digest: str) -> None:
    checksum_path = artifact.with_name(f"{artifact.name}.sha256")
    checksum = _read_regular_file(checksum_path, 256, "SHA-256 sidecar")
    expected_checksum = f"{digest}  {artifact.name}\n".encode("ascii")
    if checksum != expected_checksum:
        raise ReleaseError("SHA-256 sidecar does not match the archive")


def _validate_outer_files(
    artifact: Path,
    digest: str,
    manifest: Mapping[str, Any],
    payload: Mapping[str, tuple[bytes, int]],
    source_parameters: Mapping[str, Any],
    source_material: Mapping[str, Any],
) -> None:
    provenance_path = artifact.with_name(f"{artifact.name}.provenance.json")
    provenance = _load_json_file(provenance_path, "archive provenance")
    build_definition = _validated_provenance_definition(
        provenance,
        expected_build_type="https://devicerail.dev/build-types/portable-archive-v1",
        source_date_epoch=manifest["sourceDateEpoch"],
        label="archive provenance",
    )
    if provenance.get("subject") != [{"name": artifact.name, "digest": {"sha256": digest}}]:
        raise ReleaseError("archive provenance subject does not match the archive")
    parameters = build_definition["externalParameters"]
    expected = {
        "version": manifest["version"],
        "platform": manifest["platform"],
        "architecture": manifest["architecture"],
        "releaseStatus": manifest["releaseStatus"],
        "sourceDateEpoch": manifest["sourceDateEpoch"],
        **source_parameters,
    }
    if parameters != expected:
        raise ReleaseError("archive provenance parameters differ from the manifest")
    resolved_dependencies = build_definition["resolvedDependencies"]
    binary_dependencies = [
        {
            "uri": f"file:{name}",
            "digest": {"sha256": _sha256_bytes(data)},
        }
        for name, (data, _mode) in sorted(payload.items())
        if name.startswith("bin/devicerail-")
    ]
    if resolved_dependencies != [source_material, *binary_dependencies]:
        raise ReleaseError("archive provenance materials differ from the payload")


def _materialize_payload(
    directory: Path, payload: Mapping[str, tuple[bytes, int]]
) -> dict[str, Path]:
    materialized: dict[str, Path] = {}
    for relative, (data, mode) in payload.items():
        safe = _safe_relative_path(relative)
        target = directory.joinpath(*safe.parts)
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        try:
            with target.open("xb") as output:
                output.write(data)
            target.chmod(mode)
        except OSError as error:
            raise ReleaseError("could not materialize verified payload") from error
        materialized[relative] = target
    return materialized


def _verify_signed_release(
    sidecar_artifact: Path,
    exact_artifact: Path,
    manifest: Mapping[str, Any],
    payload: Mapping[str, tuple[bytes, int]],
    trusted_artifact_public_key: Path | None,
    runner: Runner,
    expected_macos_team_id: str | None,
    expected_macos_designated_requirement: str | None,
    expected_macos_signing_identity: str | None,
    expected_windows_publisher_subject: str | None,
    expected_windows_publisher_sha256: str | None,
) -> None:
    signing = manifest["signing"]
    expected_keys = {
        "status",
        "payloadScheme",
        "payloadSignedFiles",
        "archiveScheme",
        "archiveSignatureRequired",
        "archivePublicKeySha256",
    }
    if manifest["platform"] == "linux":
        expected_keys.add("payloadPublicKeySha256")
    if manifest["platform"] == "macos":
        expected_keys.update(
            {
                "macosTeamId",
                "macosDesignatedRequirement",
                "macosSigningIdentity",
            }
        )
    if manifest["platform"] == "windows":
        expected_keys.update(
            {
                "windowsPublisherSubject",
                "windowsPublisherSha256",
            }
        )
    if set(signing) != expected_keys:
        raise ReleaseError("signed release declaration has unknown or missing fields")
    expected_scheme = {
        "linux": "cosign-key",
        "macos": "apple-codesign",
        "windows": "authenticode",
    }[manifest["platform"]]
    binary_names = sorted(name for name in payload if name.startswith("bin/devicerail-"))
    if (
        signing.get("status") != "signed"
        or signing.get("payloadScheme") != expected_scheme
        or signing.get("payloadSignedFiles") != binary_names
        or signing.get("archiveScheme") != "cosign-key"
        or signing.get("archiveSignatureRequired") is not True
    ):
        raise ReleaseError("signed release declaration is inconsistent")
    if manifest["platform"] == "macos":
        _validate_macos_signing_config(
            expected_macos_team_id,
            expected_macos_designated_requirement,
            expected_macos_signing_identity,
        )
        if (
            signing.get("macosTeamId") != expected_macos_team_id
            or signing.get("macosDesignatedRequirement")
            != expected_macos_designated_requirement
            or signing.get("macosSigningIdentity") != expected_macos_signing_identity
        ):
            raise ReleaseError(
                "macOS signing declaration differs from the out-of-band expected identity"
            )
    elif any(
        value is not None
        for value in (
            expected_macos_team_id,
            expected_macos_designated_requirement,
            expected_macos_signing_identity,
        )
    ):
        raise ReleaseError("macOS signing expectations are invalid for this platform")

    if manifest["platform"] == "windows":
        _validate_windows_signing_config(
            expected_windows_publisher_subject,
            expected_windows_publisher_sha256,
        )
        assert expected_windows_publisher_sha256 is not None
        expected_windows_publisher_sha256 = expected_windows_publisher_sha256.lower()
        if (
            signing.get("windowsPublisherSubject")
            != expected_windows_publisher_subject
            or signing.get("windowsPublisherSha256")
            != expected_windows_publisher_sha256
        ):
            raise ReleaseError(
                "Windows signing declaration differs from the out-of-band expected identity"
            )
    elif any(
        value is not None
        for value in (
            expected_windows_publisher_subject,
            expected_windows_publisher_sha256,
        )
    ):
        raise ReleaseError("Windows signing expectations are invalid for this platform")

    if trusted_artifact_public_key is None:
        raise ReleaseError("signed release verification requires an out-of-band trusted public key")
    trusted_public_key = _read_regular_file(
        trusted_artifact_public_key, 64 * 1024, "trusted archive public key"
    )
    public_key_path = sidecar_artifact.with_name(f"{sidecar_artifact.name}.cosign.pub")
    signature_path = sidecar_artifact.with_name(f"{sidecar_artifact.name}.sig")
    public_key = _read_regular_file(public_key_path, 64 * 1024, "archive public key")
    signature = _read_regular_file(signature_path, 64 * 1024, "archive signature")
    if public_key != trusted_public_key:
        raise ReleaseError("archive public key does not match the out-of-band trust anchor")
    if _sha256_bytes(trusted_public_key) != signing.get("archivePublicKeySha256"):
        raise ReleaseError("archive public key differs from the signed declaration")
    exact_public_key = exact_artifact.parent / "trusted-archive-key.pub"
    exact_signature = exact_artifact.parent / "archive.sig"
    exact_public_key.write_bytes(trusted_public_key)
    exact_signature.write_bytes(signature)
    runner(
        [
            "cosign",
            "verify-blob",
            "--key",
            str(exact_public_key),
            "--signature",
            str(exact_signature),
            str(exact_artifact),
        ]
    )

    with tempfile.TemporaryDirectory(prefix="devicerail-verify-") as temporary:
        materialized = _materialize_payload(Path(temporary), payload)
        daemon_name = next(name for name in binary_names if "daemon" in name)
        bundle_name = next(name for name in binary_names if "bundle" in name)
        if manifest["platform"] == "linux":
            public = materialized.get("signatures/cosign.pub")
            daemon_signature = materialized.get("signatures/devicerail-daemon.sig")
            bundle_signature = materialized.get("signatures/devicerail-bundle.sig")
            if public is None or daemon_signature is None or bundle_signature is None:
                raise ReleaseError("Linux payload signatures are missing")
            if _sha256_bytes(public.read_bytes()) != signing.get("payloadPublicKeySha256"):
                raise ReleaseError("Linux payload public key differs from the declaration")
        else:
            public = daemon_signature = bundle_signature = None
        _verify_payload_signatures(
            manifest["platform"],
            materialized[daemon_name],
            materialized[bundle_name],
            public,
            daemon_signature,
            bundle_signature,
            runner,
            macos_team_id=expected_macos_team_id,
            macos_designated_requirement=expected_macos_designated_requirement,
            macos_signing_identity=expected_macos_signing_identity,
            windows_publisher_subject=expected_windows_publisher_subject,
            windows_publisher_sha256=expected_windows_publisher_sha256,
        )
        if _host_platform() != manifest["platform"]:
            raise ReleaseError(
                "signed binary embedded versions require verification on the target operating system"
            )
        _binary_version(
            materialized[daemon_name],
            "devicerail-daemon",
            manifest["version"],
        )
        _binary_version(
            materialized[bundle_name],
            "devicerail-bundle",
            manifest["version"],
        )


def _verify_unsigned_release(artifact: Path, manifest: Mapping[str, Any]) -> None:
    signing = manifest["signing"]
    if (
        not isinstance(signing, dict)
        or set(signing)
        != {
            "status",
            "payloadScheme",
            "payloadSignedFiles",
            "archiveScheme",
            "archiveSignatureRequired",
        }
        or signing.get("status") != "unsigned-test-only"
        or signing.get("payloadScheme") != "none"
        or signing.get("payloadSignedFiles") != []
        or signing.get("archiveScheme") != "none"
        or signing.get("archiveSignatureRequired") is not False
    ):
        raise ReleaseError("unsigned release declaration is inconsistent")
    for suffix in (".sig", ".cosign.pub"):
        if artifact.with_name(f"{artifact.name}{suffix}").exists():
            raise ReleaseError("unsigned artifact has ambiguous signature sidecars")


def verify_release(
    artifact: Path,
    trusted_artifact_public_key: Path | None = None,
    runner: Runner = _run_checked,
    expected_macos_team_id: str | None = None,
    expected_macos_designated_requirement: str | None = None,
    expected_macos_signing_identity: str | None = None,
    expected_windows_publisher_subject: str | None = None,
    expected_windows_publisher_sha256: str | None = None,
) -> dict[str, Any]:
    artifact_data = _read_regular_file(artifact, MAX_ARCHIVE_BYTES, "release archive")
    digest = _sha256_bytes(artifact_data)
    size = len(artifact_data)
    _validate_checksum_sidecar(artifact, digest)
    with tempfile.TemporaryDirectory(prefix="devicerail-archive-verify-") as temporary:
        exact_artifact = Path(temporary) / artifact.name
        exact_artifact.write_bytes(artifact_data)
        entries = _read_archive(exact_artifact)
        _, manifest, payload = _validate_manifest(exact_artifact, entries)
        source_parameters, source_material = _validate_embedded_metadata(
            manifest,
            payload,
        )
        if manifest["releaseStatus"] == "signed":
            _require_signed_source_claim(source_parameters, source_material)
        _validate_outer_files(
            artifact,
            digest,
            manifest,
            payload,
            source_parameters,
            source_material,
        )
        if manifest["releaseStatus"] == "signed":
            _verify_signed_release(
                artifact,
                exact_artifact,
                manifest,
                payload,
                trusted_artifact_public_key,
                runner,
                expected_macos_team_id,
                expected_macos_designated_requirement,
                expected_macos_signing_identity,
                expected_windows_publisher_subject,
                expected_windows_publisher_sha256,
            )
        else:
            _verify_unsigned_release(artifact, manifest)
    return {
        "ok": True,
        "artifact": str(artifact.resolve()),
        "sha256": digest,
        "size": size,
        "version": manifest["version"],
        "platform": manifest["platform"],
        "architecture": manifest["architecture"],
        "releaseStatus": manifest["releaseStatus"],
        "authenticated": manifest["releaseStatus"] == "signed",
        "binaryVersionVerification": (
            "executed-after-signature"
            if manifest["releaseStatus"] == "signed"
            else "package-contract-only-unsigned"
        ),
    }


def _source_date_epoch(value: str | None) -> int:
    if value is None:
        value = os.environ.get("SOURCE_DATE_EPOCH")
    if value is None or not re.fullmatch(r"[0-9]+", value):
        raise argparse.ArgumentTypeError(
            "set --source-date-epoch or SOURCE_DATE_EPOCH to a non-negative integer"
        )
    return int(value)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="devicerail-release")
    subparsers = parser.add_subparsers(dest="command", required=True)

    package = subparsers.add_parser("package", help="build a deterministic portable archive")
    package.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[1])
    package.add_argument("--platform", choices=PLATFORMS, required=True)
    package.add_argument("--architecture", choices=ARCHITECTURES, required=True)
    package.add_argument("--daemon", type=Path, required=True)
    package.add_argument("--bundle", type=Path, required=True)
    package.add_argument("--output-dir", type=Path, required=True)
    package.add_argument("--source-date-epoch", type=_source_date_epoch)
    package.add_argument("--source-uri")
    package.add_argument(
        "--release-status", choices=("signed", "unsigned-test-only"), default="unsigned-test-only"
    )
    package.add_argument("--linux-public-key", type=Path)
    package.add_argument("--linux-daemon-signature", type=Path)
    package.add_argument("--linux-bundle-signature", type=Path)
    package.add_argument("--artifact-public-key", type=Path)
    package.add_argument("--macos-team-id")
    package.add_argument("--macos-designated-requirement")
    package.add_argument("--macos-signing-identity")
    package.add_argument("--windows-publisher-subject")
    package.add_argument("--windows-publisher-sha256")
    package.set_defaults(test_cargo_metadata=None)

    verify = subparsers.add_parser("verify", help="verify archive, inventory, and signatures")
    verify.add_argument("artifact", type=Path)
    verify.add_argument("--trusted-artifact-public-key", type=Path)
    verify.add_argument("--expected-macos-team-id")
    verify.add_argument("--expected-macos-designated-requirement")
    verify.add_argument("--expected-macos-signing-identity")
    verify.add_argument("--expected-windows-publisher-subject")
    verify.add_argument("--expected-windows-publisher-sha256")
    return parser


def main(arguments: Iterable[str] | None = None) -> int:
    parser = _parser()
    try:
        args = parser.parse_args(arguments)
        if args.command == "package":
            if args.source_date_epoch is None:
                args.source_date_epoch = _source_date_epoch(None)
            summary = package_release(args)
        else:
            summary = verify_release(
                args.artifact,
                args.trusted_artifact_public_key,
                expected_macos_team_id=args.expected_macos_team_id,
                expected_macos_designated_requirement=args.expected_macos_designated_requirement,
                expected_macos_signing_identity=args.expected_macos_signing_identity,
                expected_windows_publisher_subject=args.expected_windows_publisher_subject,
                expected_windows_publisher_sha256=args.expected_windows_publisher_sha256,
            )
        sys.stdout.buffer.write(_canonical_json(summary))
        return 0
    except ReleaseError as error:
        print(f"devicerail-release: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

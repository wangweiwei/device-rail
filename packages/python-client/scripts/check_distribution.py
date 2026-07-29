#!/usr/bin/env python3
"""Verify that built Python distributions contain the generated contract."""

from __future__ import annotations

import argparse
import importlib.util
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile
import zipfile


EXPECTED_SCHEMA_DOCUMENTS = 174
EXPECTED_GENERATED_FILES = 353
MAX_SDIST_MEMBERS = 2_000
MAX_SDIST_BYTES = 16 * 1024 * 1024


RUNTIME_CHECK = r"""
import sys

wheel = sys.argv[1]
sys.path.insert(0, wheel)
for dependency_path in reversed(sys.argv[2:]):
    sys.path.insert(1, dependency_path)

import devicerail
from devicerail.schema import schema_document, schema_manifest

origin = devicerail.__file__ or ""
if wheel not in origin:
    raise RuntimeError(f"devicerail was imported from {origin!r}, not {wheel!r}")
manifest = schema_manifest()
documents = manifest.get("documents")
if not isinstance(documents, list) or len(documents) != 174:
    raise RuntimeError("installed wheel does not expose 174 Schema documents")
for item in documents:
    if not isinstance(item, dict) or not isinstance(item.get("file"), str):
        raise RuntimeError("installed Schema manifest is malformed")
    document = schema_document(item["file"])
    if not isinstance(document, dict) or "$schema" not in document:
        raise RuntimeError(f"installed Schema {item['file']} is unreadable")
if devicerail.default_hello()["protocol"]["ranges"] != [
    {"major": 1, "minMinor": 0, "maxMinor": 5}
]:
    raise RuntimeError("installed client API is inconsistent")
"""


def _runtime_import_wheel(path: Path, label: str) -> None:
    wheel = str(path.resolve())
    jsonschema_spec = importlib.util.find_spec("jsonschema")
    if jsonschema_spec is None or jsonschema_spec.origin is None:
        raise RuntimeError("jsonschema must be installed before distribution verification")
    dependency_root = str(Path(jsonschema_spec.origin).resolve().parents[1])
    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in {"PYTHONHOME", "PYTHONPATH"}
    }
    try:
        completed = subprocess.run(
            [
                sys.executable,
                "-I",
                "-c",
                RUNTIME_CHECK,
                wheel,
                dependency_root,
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=60,
            env=environment,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError(f"could not import the {label} wheel in isolation") from error
    if completed.returncode != 0:
        diagnostic = (completed.stdout + completed.stderr)[-4_096:]
        raise RuntimeError(f"{label} wheel failed its isolated runtime import:\n{diagnostic}")


def check_wheel(path: Path) -> None:
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        schemas = [name for name in names if name.endswith(".schema.json")]
        if len(schemas) != EXPECTED_SCHEMA_DOCUMENTS:
            raise RuntimeError(f"{path.name} contains {len(schemas)} Schema documents")
        generated = [
            name
            for name in names
            if name.startswith("devicerail/protocol/v1/_generated/")
            and (name.endswith(".py") or name.endswith(".json"))
        ]
        if len(generated) != EXPECTED_GENERATED_FILES:
            raise RuntimeError(f"{path.name} contains {len(generated)} generated files")
        required = {
            "devicerail/py.typed",
            "devicerail/protocol/v1/_generated/schemas/manifest.json",
        }
        missing = required - set(names)
        if missing:
            raise RuntimeError(f"{path.name} is missing {sorted(missing)}")
        metadata_name = next(name for name in names if name.endswith(".dist-info/METADATA"))
        metadata = archive.read(metadata_name).decode("utf-8")
        if "Requires-Dist: jsonschema" not in metadata:
            raise RuntimeError(f"{path.name} omits its jsonschema runtime dependency")
    _runtime_import_wheel(path, "built")


def check_sdist(path: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="devicerail-python-sdist-") as temporary:
        extraction = Path(temporary) / "source"
        wheels = Path(temporary) / "wheels"
        extraction.mkdir()
        wheels.mkdir()
        total = 0
        roots: set[str] = set()
        with tarfile.open(path, "r:gz") as archive:
            members = archive.getmembers()
            if not members or len(members) > MAX_SDIST_MEMBERS:
                raise RuntimeError(f"{path.name} has an invalid source member count")
            names = [member.name for member in members]
            schemas = [name for name in names if name.endswith(".schema.json")]
            if len(schemas) != EXPECTED_SCHEMA_DOCUMENTS:
                raise RuntimeError(f"{path.name} contains {len(schemas)} Schema documents")
            generated = [
                name
                for name in names
                if "/src/devicerail/protocol/v1/_generated/" in name
                and (name.endswith(".py") or name.endswith(".json"))
            ]
            if len(generated) != EXPECTED_GENERATED_FILES:
                raise RuntimeError(f"{path.name} contains {len(generated)} generated files")
            if not any(name.endswith("/pyproject.toml") for name in names):
                raise RuntimeError(f"{path.name} omits pyproject.toml")
            for member in members:
                relative = PurePosixPath(member.name)
                if (
                    relative.is_absolute()
                    or not relative.parts
                    or any(part in ("", ".", "..") for part in relative.parts)
                    or not (member.isdir() or member.isreg())
                ):
                    raise RuntimeError(f"{path.name} contains an unsafe source member")
                roots.add(relative.parts[0])
                total += member.size
                if total > MAX_SDIST_BYTES:
                    raise RuntimeError(f"{path.name} expands beyond the source size limit")
                target = extraction.joinpath(*relative.parts)
                if member.isdir():
                    target.mkdir(parents=True, exist_ok=True)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(member)
                if source is None:
                    raise RuntimeError(f"{path.name} contains an unreadable source member")
                data = source.read(MAX_SDIST_BYTES + 1)
                if len(data) != member.size:
                    raise RuntimeError(f"{path.name} contains an inconsistent source member")
                target.write_bytes(data)
        if len(roots) != 1:
            raise RuntimeError(f"{path.name} must contain one source root")
        source_root = extraction / next(iter(roots))
        environment = {
            key: value
            for key, value in os.environ.items()
            if key not in {"PYTHONHOME", "PYTHONPATH"}
        }
        try:
            completed = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "build",
                    "--wheel",
                    "--no-isolation",
                    "--outdir",
                    str(wheels),
                    str(source_root),
                ],
                cwd=temporary,
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=120,
                env=environment,
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise RuntimeError(f"could not build {path.name} without network isolation") from error
        if completed.returncode != 0:
            diagnostic = (completed.stdout + completed.stderr)[-4_096:]
            raise RuntimeError(f"{path.name} failed its isolated wheel build:\n{diagnostic}")
        rebuilt = sorted(wheels.glob("*.whl"))
        if len(rebuilt) != 1:
            raise RuntimeError(f"{path.name} did not build exactly one wheel")
        check_wheel(rebuilt[0])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("dist", type=Path)
    args = parser.parse_args()
    wheels = sorted(args.dist.glob("*.whl"))
    sdists = sorted(args.dist.glob("*.tar.gz"))
    if len(wheels) != 1 or len(sdists) != 1:
        raise RuntimeError(
            f"expected one wheel and one sdist, found {len(wheels)} and {len(sdists)}"
        )
    check_wheel(wheels[0])
    check_sdist(sdists[0])
    print(f"verified {wheels[0].name} and {sdists[0].name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

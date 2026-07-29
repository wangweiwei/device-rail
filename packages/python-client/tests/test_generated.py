from __future__ import annotations

import json
import importlib
import pkgutil
import subprocess
import sys
import typing
import unittest
from importlib.resources import files
from pathlib import Path

from devicerail.protocol.v1 import METHOD_SPECS
import devicerail.protocol.v1._generated.models as generated_models
from devicerail.protocol.v1._generated.models.device_connect_request import NoParamsSchema
from devicerail.schema import schema_manifest, validate_document
from devicerail.errors import ProtocolViolationError


PACKAGE_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = PACKAGE_ROOT.parents[1]


class GeneratedProtocolTests(unittest.TestCase):
    def test_generator_is_deterministic_and_complete(self) -> None:
        completed = subprocess.run(
            [sys.executable, "scripts/generate.py", "--check"],
            cwd=PACKAGE_ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        self.assertEqual(len(schema_manifest()["documents"]), 174)
        generated_root = (
            PACKAGE_ROOT
            / "src"
            / "devicerail"
            / "protocol"
            / "v1"
            / "_generated"
        )
        generated_files = [
            path
            for path in generated_root.rglob("*")
            if path.is_file()
            and "__pycache__" not in path.parts
            and path.suffix in {".json", ".py"}
        ]
        self.assertEqual(len(generated_files), 353)
        self.assertEqual(len(METHOD_SPECS), 24)
        self.assertEqual(sum(spec.websocket_only for spec in METHOD_SPECS.values()), 1)

    def test_all_golden_fixtures_validate_against_packaged_schema(self) -> None:
        fixture_root = REPOSITORY_ROOT / "crates" / "protocol" / "fixtures"
        manifest = json.loads((fixture_root / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(len(manifest["fixtures"]), 89)
        for fixture in manifest["fixtures"]:
            with self.subTest(fixture=fixture["id"]):
                instance = json.loads(
                    (fixture_root / fixture["path"]).read_text(encoding="utf-8")
                )
                validate_document(Path(fixture["schema"]).name, instance)

    def test_packaged_schemas_are_byte_identical_to_the_source(self) -> None:
        source = REPOSITORY_ROOT / "protocol" / "schema" / "v1"
        packaged = files("devicerail.protocol.v1._generated.schemas")
        for document in schema_manifest()["documents"]:
            with self.subTest(schema=document["file"]):
                self.assertEqual(
                    packaged.joinpath(document["file"]).read_bytes(),
                    (source / document["file"]).read_bytes(),
                )

    def test_every_generated_typed_dict_resolves_its_forward_references(self) -> None:
        resolved = 0
        for module_info in pkgutil.iter_modules(generated_models.__path__):
            module = importlib.import_module(
                f"{generated_models.__name__}.{module_info.name}"
            )
            for value in vars(module).values():
                if (
                    isinstance(value, type)
                    and typing.is_typeddict(value)
                    and value.__module__ == module.__name__
                ):
                    typing.get_type_hints(value, vars(module), vars(module))
                    resolved += 1
        self.assertEqual(resolved, 959)

    def test_empty_array_schema_is_not_widened_to_an_arbitrary_list(self) -> None:
        array_branches = [
            branch
            for branch in typing.get_args(NoParamsSchema)
            if typing.get_origin(branch) is list
        ]
        self.assertEqual(len(array_branches), 1)
        self.assertEqual(typing.get_args(array_branches[0]), (typing.Never,))
        validate_document(
            "device-connect-request.schema.json",
            {"jsonrpc": "2.0", "id": "empty", "method": "device.connect", "params": []},
        )
        with self.assertRaises(ProtocolViolationError):
            validate_document(
                "device-connect-request.schema.json",
                {
                    "jsonrpc": "2.0",
                    "id": "non-empty",
                    "method": "device.connect",
                    "params": [1],
                },
            )


if __name__ == "__main__":
    unittest.main()

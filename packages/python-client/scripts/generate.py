#!/usr/bin/env python3
"""Generate Python protocol types and resources from the checked-in v1 Schema."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import tempfile
from collections.abc import Iterable
from pathlib import Path
from typing import Any


PACKAGE_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = PACKAGE_ROOT.parents[1]
SCHEMA_ROOT = REPOSITORY_ROOT / "protocol" / "schema" / "v1"
OUTPUT_ROOT = PACKAGE_ROOT / "src" / "devicerail" / "protocol" / "v1" / "_generated"
HEADER = """# Generated from protocol/schema/v1. DO NOT EDIT.\n# Run `python scripts/generate.py` from packages/python-client.\n"""


def snake_case(value: str) -> str:
    return value.replace("-", "_")


def pascal_case(value: str) -> str:
    words = re.findall(r"[A-Z]+(?=[A-Z][a-z]|\d|$)|[A-Z]?[a-z]+|\d+", value)
    return "".join(word[:1].upper() + word[1:] for word in words) or "Value"


def union(expressions: Iterable[str]) -> str:
    unique = list(dict.fromkeys(expressions))
    if "Any" in unique:
        return "Any"
    if not unique:
        return "Never"
    if len(unique) == 1:
        return unique[0]
    return " | ".join(unique)


class ModelRenderer:
    def __init__(self, schema: dict[str, Any], title: str) -> None:
        self.schema = schema
        self.title = title
        self.definitions: dict[str, dict[str, Any]] = dict(schema.get("$defs", {}))
        self.names: dict[int, str] = {}
        self.used_names: set[str] = set(self.definitions)
        for name, definition in self.definitions.items():
            self.names[id(definition)] = name
        self.names[id(schema)] = title
        self.used_names.add(title)
        for name, definition in self.definitions.items():
            self._discover(definition, name)
        self._discover(schema, title)

    def _unique_name(self, candidate: str) -> str:
        base = pascal_case(candidate)
        name = base
        suffix = 2
        while name in self.used_names:
            name = f"{base}{suffix}"
            suffix += 1
        self.used_names.add(name)
        return name

    def _discover(self, value: Any, path: str) -> None:
        if isinstance(value, list):
            for index, item in enumerate(value, start=1):
                self._discover(item, f"{path}Item{index}")
            return
        if not isinstance(value, dict):
            return
        for combinator in ("anyOf", "oneOf"):
            for index, branch in enumerate(value.get(combinator, []), start=1):
                if isinstance(branch, dict) and branch.get("type") == "object":
                    self.names.setdefault(
                        id(branch), self._unique_name(f"{path}Variant{index}")
                    )
                self._discover(branch, f"{path}Variant{index}")
        properties = value.get("properties", {})
        if isinstance(properties, dict):
            for key, child in properties.items():
                if isinstance(child, dict) and child.get("type") == "object":
                    self.names.setdefault(
                        id(child), self._unique_name(f"{path}{pascal_case(key)}")
                    )
                self._discover(child, f"{path}{pascal_case(key)}")
        items = value.get("items")
        if isinstance(items, dict):
            if items.get("type") == "object":
                self.names.setdefault(id(items), self._unique_name(f"{path}Item"))
            self._discover(items, f"{path}Item")

    def _resolve_ref(self, reference: str) -> str:
        prefix = "#/$defs/"
        if not reference.startswith(prefix):
            raise ValueError(f"unsupported non-local Schema reference: {reference}")
        name = reference.removeprefix(prefix)
        if name not in self.definitions:
            raise ValueError(f"unknown Schema definition: {reference}")
        return name

    def expression(self, schema: Any) -> str:
        if schema is True or schema == {}:
            return "Any"
        if schema is False:
            return "Never"
        if not isinstance(schema, dict):
            raise TypeError(f"invalid Schema node: {schema!r}")
        if "$ref" in schema:
            return self._resolve_ref(schema["$ref"])
        if "const" in schema:
            return f"Literal[{schema['const']!r}]"
        if "enum" in schema:
            return f"Literal[{', '.join(repr(item) for item in schema['enum'])}]"
        for combinator in ("anyOf", "oneOf"):
            if combinator in schema:
                return union(self.expression(branch) for branch in schema[combinator])
        schema_type = schema.get("type")
        if isinstance(schema_type, list):
            return union(self.expression({**schema, "type": item}) for item in schema_type)
        if schema_type == "string":
            return "str"
        if schema_type == "integer":
            return "int"
        if schema_type == "number":
            return "int | float"
        if schema_type == "boolean":
            return "bool"
        if schema_type == "null":
            return "None"
        if schema_type == "array":
            maximum = schema.get("maxItems")
            items = schema.get("items", True)
            prefix_items = schema.get("prefixItems")
            if prefix_items is not None:
                raise ValueError("tuple-style array Schema is not supported")
            empty_only = items is False or (
                isinstance(maximum, int)
                and not isinstance(maximum, bool)
                and maximum == 0
            )
            if empty_only:
                minimum = schema.get("minItems", 0)
                if (
                    not isinstance(minimum, int)
                    or isinstance(minimum, bool)
                    or minimum != 0
                ):
                    raise ValueError("array Schema requires items but forbids every item")
                return "list[Never]"
            return f"list[{self.expression(items)}]"
        if schema_type == "object":
            name = self.names.get(id(schema))
            if name and ("properties" in schema or schema.get("additionalProperties") is False):
                return name
            additional = schema.get("additionalProperties", True)
            value_type = self.expression(additional) if isinstance(additional, dict) else "Any"
            return f"dict[str, {value_type}]"
        return "Any"

    def _object_classes(self) -> list[tuple[str, dict[str, Any]]]:
        objects: list[tuple[str, dict[str, Any]]] = []
        seen: set[int] = set()

        def visit(value: Any) -> None:
            if isinstance(value, list):
                for item in value:
                    visit(item)
                return
            if not isinstance(value, dict) or id(value) in seen:
                return
            seen.add(id(value))
            name = self.names.get(id(value))
            if (
                name
                and value.get("type") == "object"
                and ("properties" in value or value.get("additionalProperties") is False)
            ):
                objects.append((name, value))
            for child in value.values():
                visit(child)

        visit(self.schema)
        return sorted(objects, key=lambda item: item[0])

    def _alias_definitions(self) -> list[tuple[str, dict[str, Any]]]:
        aliases = [
            (name, definition)
            for name, definition in self.definitions.items()
            if not (
                definition.get("type") == "object"
                and ("properties" in definition or definition.get("additionalProperties") is False)
            )
        ]
        if not (
            self.schema.get("type") == "object"
            and ("properties" in self.schema or self.schema.get("additionalProperties") is False)
        ):
            aliases.append((self.title, self.schema))

        dependencies: dict[str, set[str]] = {}
        alias_names = {name for name, _ in aliases}
        for name, definition in aliases:
            refs: set[str] = set()

            def collect(value: Any) -> None:
                if isinstance(value, dict):
                    reference = value.get("$ref")
                    if isinstance(reference, str) and reference.startswith("#/$defs/"):
                        target = reference.rsplit("/", 1)[-1]
                        if target in alias_names and target != name:
                            refs.add(target)
                    for child in value.values():
                        collect(child)
                elif isinstance(value, list):
                    for child in value:
                        collect(child)

            collect(definition)
            dependencies[name] = refs

        ordered: list[tuple[str, dict[str, Any]]] = []
        remaining = dict(aliases)
        while remaining:
            ready = sorted(
                name for name in remaining if not (dependencies[name] & remaining.keys())
            )
            if not ready:
                raise ValueError(f"cyclic Schema aliases in {self.title}: {sorted(remaining)}")
            for name in ready:
                ordered.append((name, remaining.pop(name)))
        return ordered

    def render(self, schema_file: str) -> str:
        lines = [
            HEADER.rstrip(),
            "from __future__ import annotations",
            "",
            "from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict",
            "",
            f"# Source: protocol/schema/v1/{schema_file}",
            "",
        ]
        for name, definition in self._object_classes():
            lines.append(f"class {name}(TypedDict):")
            properties = definition.get("properties", {})
            required = set(definition.get("required", []))
            if not properties:
                lines.append("    pass")
            else:
                for key, child in properties.items():
                    expression = self.expression(child)
                    if key not in required:
                        expression = f"NotRequired[{expression}]"
                    lines.append(f"    {key}: {expression}")
            lines.append("")
        for name, definition in self._alias_definitions():
            expression = self.expression_without_self(definition, name)
            lines.extend((f"{name}: TypeAlias = {expression}", ""))
        lines.extend((f"__all__ = [{self.title!r}]", ""))
        return "\n".join(lines)

    def expression_without_self(self, schema: dict[str, Any], name: str) -> str:
        original = self.names.pop(id(schema), None)
        try:
            expression = self.expression(schema)
        finally:
            if original is not None:
                self.names[id(schema)] = original
        if expression == name:
            raise ValueError(f"self-referential alias {name}")
        return expression


def resolve_local(schema: dict[str, Any], node: dict[str, Any]) -> dict[str, Any]:
    while "$ref" in node:
        reference = node["$ref"]
        if not reference.startswith("#/$defs/"):
            raise ValueError(f"unsupported reference: {reference}")
        node = schema["$defs"][reference.rsplit("/", 1)[-1]]
    return node


def method_from_request(schema: dict[str, Any]) -> str | None:
    method_schema = schema.get("properties", {}).get("method")
    if not isinstance(method_schema, dict):
        return None
    resolved = resolve_local(schema, method_schema)
    values = resolved.get("enum")
    if isinstance(values, list) and len(values) == 1 and isinstance(values[0], str):
        return values[0]
    value = resolved.get("const")
    return value if isinstance(value, str) else None


def referenced_name(node: dict[str, Any]) -> str:
    reference = node.get("$ref")
    if not isinstance(reference, str) or not reference.startswith("#/$defs/"):
        raise ValueError(f"method type must use a local named definition: {node}")
    return reference.rsplit("/", 1)[-1]


def result_reference(response: dict[str, Any]) -> str | None:
    definitions = response.get("$defs", {})
    candidates = [
        value
        for name, value in definitions.items()
        if name.endswith("SuccessSchema")
        and isinstance(value, dict)
        and "result" in value.get("properties", {})
    ]
    if len(candidates) != 1:
        raise ValueError(f"expected one success result in {response.get('title')}")
    result = candidates[0]["properties"]["result"]
    reference = result.get("$ref")
    if isinstance(reference, str) and reference.startswith("#/$defs/"):
        return reference.rsplit("/", 1)[-1]
    return None


def render_methods(documents: list[dict[str, str]], schemas: dict[str, dict[str, Any]]) -> str:
    by_method: dict[str, dict[str, Any]] = {}
    file_by_title = {document["name"]: document["file"] for document in documents}
    for document in documents:
        file_name = document["file"]
        if not file_name.endswith("-request.schema.json") or file_name == "rpc-request.schema.json":
            continue
        request = schemas[file_name]
        method = method_from_request(request)
        if method is None:
            continue
        response_file = file_name.replace("-request.schema.json", "-response.schema.json")
        response = schemas[response_file]
        params_node = request.get("properties", {}).get("params")
        params_name = referenced_name(params_node) if params_node else None
        canonical_params_file = file_by_title.get(params_name) if params_name else None
        params_module = snake_case(
            (canonical_params_file or file_name).removesuffix(".schema.json")
        )
        standalone_result_file = file_name.replace(
            "-request.schema.json", "-result.schema.json"
        )
        if standalone_result_file not in schemas and method == "system.hello":
            standalone_result_file = "hello-result.schema.json"
        if standalone_result_file not in schemas:
            referenced_result = result_reference(response)
            if referenced_result is None:
                raise ValueError(f"no named result document for {method}")
            standalone_result_file = file_by_title[referenced_result]
        result_module = snake_case(
            standalone_result_file.removesuffix(".schema.json")
        )
        result_type = schemas[standalone_result_file]["title"]
        by_method[method] = {
            "method": method,
            "request_file": file_name,
            "request_module": snake_case(file_name.removesuffix(".schema.json")),
            "request_title": request["title"],
            "response_file": response_file,
            "response_module": snake_case(response_file.removesuffix(".schema.json")),
            "response_title": response["title"],
            "params_module": params_module,
            "params_name": params_name,
            "params_required": "params" in request.get("required", []),
            "result_module": result_module,
            "result_name": result_type,
            "timeout": "timeoutMs" in request.get("properties", {}),
            "websocket_only": method == "events.subscribe",
        }
    methods = [by_method[name] for name in sorted(by_method)]
    if len(methods) != 24:
        raise ValueError(f"expected 24 public RPC methods, found {len(methods)}")

    lines = [
        HEADER.rstrip(),
        "from __future__ import annotations",
        "",
        "from collections.abc import Mapping",
        "from dataclasses import dataclass",
        "from types import MappingProxyType",
        "from typing import Any, Final, Literal, TypedDict, TypeAlias, overload",
        "",
        "from devicerail.types import RequestHandle",
    ]
    for entry in methods:
        lines.append(
            f"from .models import {entry['request_module']} as _{entry['request_module']}"
        )
        lines.append(
            f"from .models import {entry['response_module']} as _{entry['response_module']}"
        )
        if entry["result_module"] != entry["response_module"]:
            lines.append(
                f"from .models import {entry['result_module']} as _{entry['result_module']}"
            )
        if entry["params_module"] not in {
            entry["request_module"],
            entry["response_module"],
            entry["result_module"],
        }:
            lines.append(
                f"from .models import {entry['params_module']} as _{entry['params_module']}"
            )
    lines.extend(("", ""))
    literals = ", ".join(repr(entry["method"]) for entry in methods)
    stdio_literals = ", ".join(
        repr(entry["method"])
        for entry in methods
        if entry["method"] != "system.hello" and not entry["websocket_only"]
    )
    lines.extend(
        (
            f"RpcMethod: TypeAlias = Literal[{literals}]",
            f"StdioRpcMethod: TypeAlias = Literal[{stdio_literals}]",
            "",
            "@dataclass(frozen=True, slots=True)",
            "class MethodSpec:",
            "    request_schema: str",
            "    response_schema: str",
            "    params_required: bool",
            "    timeout_supported: bool",
            "    websocket_only: bool",
            "",
            "METHOD_SPECS: Final[Mapping[RpcMethod, MethodSpec]] = MappingProxyType({",
        )
    )
    for entry in methods:
        lines.append(
            f"    {entry['method']!r}: MethodSpec({entry['request_file']!r}, "
            f"{entry['response_file']!r}, {entry['params_required']!r}, "
            f"{entry['timeout']!r}, {entry['websocket_only']!r}),"
        )
    lines.extend(("})", ""))

    method_entry_names: list[str] = []
    for entry in methods:
        class_name = pascal_case(entry["method"]) + "Method"
        method_entry_names.append(class_name)
        lines.extend(
            (
                f"class {class_name}(TypedDict):",
                f"    request: _{entry['request_module']}.{entry['request_title']}",
                f"    response: _{entry['response_module']}.{entry['response_title']}",
                "",
            )
        )
    lines.append("RpcMethodMap = TypedDict(")
    lines.append("    \"RpcMethodMap\",")
    lines.append("    {")
    for entry, class_name in zip(methods, method_entry_names):
        lines.append(f"        {entry['method']!r}: {class_name},")
    lines.extend(("    },", ")", ""))

    application_methods = [entry for entry in methods if entry["method"] != "system.hello"]
    lines.append("class GeneratedClientMethods:")
    for function, return_prefix in (("call", ""), ("begin_call", "RequestHandle[")):
        for entry in application_methods:
            params_type = (
                f"_{entry['params_module']}.{entry['params_name']}"
                if entry["params_name"]
                else "None"
            )
            result_type = f"_{entry['result_module']}.{entry['result_name']}"
            return_type = f"{return_prefix}{result_type}{']' if return_prefix else ''}"
            lines.append("    @overload")
            lines.append(f"    async def {function}(")
            lines.append("        self,")
            lines.append(f"        method: Literal[{entry['method']!r}],")
            if entry["params_required"]:
                lines.append(f"        params: {params_type},")
            else:
                lines.append(f"        params: {params_type} | None = None,")
            if entry["timeout"]:
                lines.append("        *,")
                lines.append("        timeout_ms: int | None = None,")
            lines.append(f"    ) -> {return_type}: ...")
            lines.append("")
        lines.append(f"    async def {function}(")
        lines.append("        self,")
        lines.append("        method: RpcMethod,")
        lines.append("        params: Any = None,")
        lines.append("        *,")
        lines.append("        timeout_ms: int | None = None,")
        lines.append("    ) -> Any:")
        target = "_call" if function == "call" else "_begin_call"
        lines.append(f"        return await self.{target}(method, params, timeout_ms=timeout_ms)")
        lines.append("")
    lines.extend(
        (
            "    async def _call(",
            "        self, method: RpcMethod, params: Any, *, timeout_ms: int | None",
            "    ) -> Any:",
            "        raise NotImplementedError",
            "",
            "    async def _begin_call(",
            "        self, method: RpcMethod, params: Any, *, timeout_ms: int | None",
            "    ) -> RequestHandle[Any]:",
            "        raise NotImplementedError",
            "",
            "__all__ = [",
            "    \"GeneratedClientMethods\",",
            "    \"METHOD_SPECS\",",
            "    \"MethodSpec\",",
            "    \"RpcMethod\",",
            "    \"RpcMethodMap\",",
            "    \"StdioRpcMethod\",",
            "]",
            "",
        )
    )
    return "\n".join(lines)


def generated_files() -> dict[Path, str | bytes]:
    manifest = json.loads((SCHEMA_ROOT / "manifest.json").read_text(encoding="utf-8"))
    documents: list[dict[str, str]] = manifest["documents"]
    listed_files = {document["file"] for document in documents}
    actual_files = {path.name for path in SCHEMA_ROOT.glob("*.schema.json")}
    if listed_files != actual_files:
        raise ValueError(
            f"Schema manifest mismatch: missing={sorted(actual_files - listed_files)}, "
            f"stale={sorted(listed_files - actual_files)}"
        )
    schemas = {
        file_name: json.loads((SCHEMA_ROOT / file_name).read_text(encoding="utf-8"))
        for file_name in sorted(listed_files)
    }
    output: dict[Path, str | bytes] = {
        Path("__init__.py"): HEADER + "\nfrom .methods import *\nfrom .models import *\n",
        Path("models/__init__.py"): HEADER + "\n",
        Path("methods.py"): render_methods(documents, schemas),
        Path("schemas/__init__.py"): HEADER,
        Path("schemas/manifest.json"): (SCHEMA_ROOT / "manifest.json").read_bytes(),
    }
    model_exports: list[str] = []
    for document in sorted(documents, key=lambda item: item["name"]):
        file_name = document["file"]
        module = snake_case(file_name.removesuffix(".schema.json"))
        title = document["name"]
        output[Path(f"models/{module}.py")] = ModelRenderer(
            schemas[file_name], title
        ).render(file_name)
        model_exports.append(f"from .{module} import {title} as {title}")
        output[Path(f"schemas/{file_name}")] = (SCHEMA_ROOT / file_name).read_bytes()
    output[Path("models/__init__.py")] += (
        "\n".join(model_exports)
        + "\n\n__all__ = [\n"
        + "".join(f"    {document['name']!r},\n" for document in sorted(documents, key=lambda item: item["name"]))
        + "]\n"
    )
    return output


def write_output(root: Path, files: dict[Path, str | bytes]) -> None:
    if root.exists():
        shutil.rmtree(root)
    for relative, content in files.items():
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            target.write_bytes(content)
        else:
            target.write_bytes(content.encode("utf-8"))


def compare_output(files: dict[Path, str | bytes]) -> list[str]:
    with tempfile.TemporaryDirectory(prefix="devicerail-python-gen-") as temp:
        expected_root = Path(temp) / "_generated"
        write_output(expected_root, files)
        expected = {
            path.relative_to(expected_root): path.read_bytes()
            for path in expected_root.rglob("*")
            if path.is_file()
        }
        actual = (
            {
                path.relative_to(OUTPUT_ROOT): path.read_bytes()
                for path in OUTPUT_ROOT.rglob("*")
                if path.is_file() and "__pycache__" not in path.parts
            }
            if OUTPUT_ROOT.exists()
            else {}
        )
    changes = [f"missing {path}" for path in sorted(expected.keys() - actual.keys())]
    changes.extend(f"stale {path}" for path in sorted(actual.keys() - expected.keys()))
    changes.extend(
        f"changed {path}"
        for path in sorted(expected.keys() & actual.keys())
        if expected[path] != actual[path]
    )
    return changes


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="fail if generated files differ")
    args = parser.parse_args()
    files = generated_files()
    if args.check:
        changes = compare_output(files)
        if changes:
            print("generated Python protocol files are out of date:")
            for change in changes:
                print(f"  {change}")
            return 1
        print(f"checked {len(files)} generated Python protocol files")
        return 0
    write_output(OUTPUT_ROOT, files)
    print(f"generated {len(files)} Python protocol files in {OUTPUT_ROOT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

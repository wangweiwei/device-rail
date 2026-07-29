"""Runtime access to the generated, packaged DeviceRail v1 Schema."""

from __future__ import annotations

import json
from copy import deepcopy
from functools import cache
from importlib.resources import files
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker
from jsonschema.exceptions import SchemaError, ValidationError

from .errors import ProtocolViolationError


@cache
def _schema_manifest() -> dict[str, Any]:
    resource = files("devicerail.protocol.v1._generated.schemas").joinpath("manifest.json")
    return json.loads(resource.read_text(encoding="utf-8"))


def schema_manifest() -> dict[str, Any]:
    return deepcopy(_schema_manifest())


@cache
def _schema_document(file_name: str) -> dict[str, Any]:
    allowed = {item["file"] for item in _schema_manifest()["documents"]}
    if file_name not in allowed:
        raise KeyError(f"unknown DeviceRail v1 Schema document: {file_name}")
    resource = files("devicerail.protocol.v1._generated.schemas").joinpath(file_name)
    return json.loads(resource.read_text(encoding="utf-8"))


def schema_document(file_name: str) -> dict[str, Any]:
    return deepcopy(_schema_document(file_name))


@cache
def _validator(file_name: str) -> Draft202012Validator:
    schema = _schema_document(file_name)
    try:
        Draft202012Validator.check_schema(schema)
    except SchemaError as error:
        raise RuntimeError(f"packaged Schema {file_name} is invalid") from error
    return Draft202012Validator(schema, format_checker=FormatChecker())


def validate_document(file_name: str, instance: object) -> None:
    """Validate an instance and raise a stable client error on the first violation."""

    errors = sorted(
        _validator(file_name).iter_errors(instance),
        key=lambda error: (list(error.absolute_path), error.message),
    )
    if not errors:
        return
    error: ValidationError = errors[0]
    location = "$" + "".join(
        f"[{part}]" if isinstance(part, int) else f".{part}" for part in error.absolute_path
    )
    raise ProtocolViolationError(
        f"{file_name} rejected {location}: {error.message}"
    ) from error


__all__ = ["schema_document", "schema_manifest", "validate_document"]

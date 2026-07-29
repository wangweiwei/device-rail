# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/feature-offer.schema.json

class FeatureOffer(TypedDict):
    optional: NotRequired[list[str]]
    required: NotRequired[list[str]]

__all__ = ['FeatureOffer']

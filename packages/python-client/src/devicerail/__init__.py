"""Public DeviceRail Python Client API."""

from .client import ClientState, DeviceRailClient, RpcId, default_hello
from .errors import (
    DeviceRailClientError,
    FeatureNotNegotiatedError,
    HandshakeStateError,
    NdjsonFrameTooLargeError,
    NdjsonIncompleteFrameError,
    NdjsonInvalidUtf8Error,
    PendingRequestLimitError,
    ProtocolViolationError,
    RpcRemoteError,
    TransportClosedError,
    WriteFrameTooLargeError,
)
from .protocol.v1 import METHOD_SPECS, RpcMethod, RpcMethodMap, StdioRpcMethod
from .types import RequestHandle

__version__ = "0.3.2"

__all__ = [
    "ClientState",
    "DeviceRailClient",
    "DeviceRailClientError",
    "FeatureNotNegotiatedError",
    "HandshakeStateError",
    "METHOD_SPECS",
    "NdjsonFrameTooLargeError",
    "NdjsonIncompleteFrameError",
    "NdjsonInvalidUtf8Error",
    "PendingRequestLimitError",
    "ProtocolViolationError",
    "RequestHandle",
    "RpcId",
    "RpcMethod",
    "RpcMethodMap",
    "RpcRemoteError",
    "StdioRpcMethod",
    "TransportClosedError",
    "WriteFrameTooLargeError",
    "default_hello",
]

"""DeviceRail Python client errors."""

from __future__ import annotations

from typing import Any


class DeviceRailClientError(Exception):
    """Base class for client and transport failures."""

    code = "client_error"


class ProtocolViolationError(DeviceRailClientError):
    code = "protocol_violation"


class HandshakeStateError(DeviceRailClientError):
    code = "handshake_state"


class FeatureNotNegotiatedError(DeviceRailClientError):
    code = "feature_not_negotiated"

    def __init__(self, method: str, feature: str) -> None:
        self.method = method
        self.feature = feature
        super().__init__(f"{method} requires negotiated protocol feature {feature}")


class PendingRequestLimitError(DeviceRailClientError):
    code = "pending_request_limit"

    def __init__(self, limit: int) -> None:
        self.limit = limit
        super().__init__(f"the client already has {limit} pending requests")


class TransportClosedError(DeviceRailClientError):
    code = "transport_closed"


class NdjsonFrameTooLargeError(DeviceRailClientError):
    code = "ndjson_frame_too_large"

    def __init__(self, limit_bytes: int, actual_bytes: int) -> None:
        self.limit_bytes = limit_bytes
        self.actual_bytes = actual_bytes
        super().__init__(
            f"NDJSON frame is {actual_bytes} bytes; the limit is {limit_bytes} bytes"
        )


class WriteFrameTooLargeError(DeviceRailClientError):
    code = "write_frame_too_large"

    def __init__(self, limit_bytes: int, actual_bytes: int) -> None:
        self.limit_bytes = limit_bytes
        self.actual_bytes = actual_bytes
        super().__init__(
            f"outbound NDJSON frame is {actual_bytes} bytes; the limit is {limit_bytes} bytes"
        )


class NdjsonInvalidUtf8Error(DeviceRailClientError):
    code = "invalid_ndjson_utf8"


class NdjsonIncompleteFrameError(DeviceRailClientError):
    code = "incomplete_ndjson_frame"

    def __init__(self, buffered_bytes: int) -> None:
        self.buffered_bytes = buffered_bytes
        super().__init__(
            f"transport ended with an incomplete {buffered_bytes}-byte NDJSON frame"
        )


class RpcRemoteError(DeviceRailClientError):
    code = "remote_rpc_error"

    def __init__(self, request_id: str | int, rpc_error: dict[str, Any]) -> None:
        self.request_id = request_id
        self.rpc_error = rpc_error
        super().__init__(str(rpc_error["message"]))

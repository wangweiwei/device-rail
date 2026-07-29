"""Typed asynchronous stdio/NDJSON client for DeviceRail."""

from __future__ import annotations

import asyncio
import copy
import json
import math
import os
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Literal, cast

from .errors import (
    FeatureNotNegotiatedError,
    HandshakeStateError,
    PendingRequestLimitError,
    ProtocolViolationError,
    RpcRemoteError,
    TransportClosedError,
    WriteFrameTooLargeError,
)
from .framing import DEFAULT_MAX_FRAME_BYTES, NdjsonDecoder
from .protocol.v1._generated.methods import (
    METHOD_SPECS,
    GeneratedClientMethods,
    RpcMethod,
)
from .protocol.v1._generated.models.hello_params import HelloParams
from .protocol.v1._generated.models.hello_result import HelloResult
from .schema import validate_document
from .types import RequestHandle


REQUEST_CONTROL_FEATURE = "request.control.v1"
ROUTING_FEATURE = "device.routing.v1"
EVENTS_FEATURE = "events.snapshot.v1"
ACTION_PROTECTED_FEATURE = "action.protected.v1"
EVENTS_STREAM_FEATURE = "events.stream.v1"
MEDIA_STREAM_FEATURE = "media.stream.v1"
SESSION_EXPORT_PAGE_FEATURE = "session.export.page.v1"
OBSERVATION_UI_SNAPSHOT_FEATURE = "observation.uiSnapshot.v1"
SEMANTIC_ACTIONS_FEATURE = "device.semanticActions.v1"
VERDICT_RECORD_FEATURE = "verdict.record.v1"
SUPPORTED_PROTOCOL_MAJOR = 1
SUPPORTED_PROTOCOL_MAX_MINOR = 5
MAX_SAFE_INTEGER = 9_007_199_254_740_991

_SEMANTIC_ACTIONS = frozenset(
    {
        "findElement",
        "tapElement",
        "clearElement",
        "setElementValue",
        "waitForElement",
    }
)

_REQUIRED_FEATURES: dict[str, str] = {
    "device.select": ROUTING_FEATURE,
    "devices.list": ROUTING_FEATURE,
    "events.clear": EVENTS_FEATURE,
    "events.list": EVENTS_FEATURE,
    "events.stream.open": EVENTS_STREAM_FEATURE,
    "media.stream.capture": MEDIA_STREAM_FEATURE,
    "media.stream.end": MEDIA_STREAM_FEATURE,
    "media.stream.start": MEDIA_STREAM_FEATURE,
    "request.cancel": REQUEST_CONTROL_FEATURE,
    "session.export": EVENTS_FEATURE,
    "sessions.list": EVENTS_FEATURE,
    "ui.snapshot.get": OBSERVATION_UI_SNAPSHOT_FEATURE,
    "verdict.record": VERDICT_RECORD_FEATURE,
}
_FEATURE_MINORS = {
    REQUEST_CONTROL_FEATURE: 1,
    ROUTING_FEATURE: 2,
    ACTION_PROTECTED_FEATURE: 2,
    EVENTS_STREAM_FEATURE: 3,
    MEDIA_STREAM_FEATURE: 4,
    SESSION_EXPORT_PAGE_FEATURE: 4,
    OBSERVATION_UI_SNAPSHOT_FEATURE: 5,
    SEMANTIC_ACTIONS_FEATURE: 5,
    VERDICT_RECORD_FEATURE: 5,
}

ClientState = Literal[
    "awaitingHello", "helloInFlight", "ready", "closing", "closed", "failed"
]
RpcId = str | int
_RpcKey = tuple[type[str] | type[int], RpcId]


def default_hello(
    *, client_name: str = "devicerail-python", client_version: str = "0.1.0"
) -> HelloParams:
    """Return the default Protocol 1.0-1.5 offer for this package."""

    return {
        "client": {"name": client_name, "version": client_version},
        "protocol": {"ranges": [{"major": 1, "minMinor": 0, "maxMinor": 5}]},
        "features": {
            "required": [],
            "optional": [
                REQUEST_CONTROL_FEATURE,
                ROUTING_FEATURE,
                EVENTS_FEATURE,
                ACTION_PROTECTED_FEATURE,
                EVENTS_STREAM_FEATURE,
                MEDIA_STREAM_FEATURE,
                SESSION_EXPORT_PAGE_FEATURE,
                OBSERVATION_UI_SNAPSHOT_FEATURE,
                SEMANTIC_ACTIONS_FEATURE,
                VERDICT_RECORD_FEATURE,
            ],
        },
    }


def _positive_safe_integer(value: int, name: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ValueError(f"{name} must be a positive integer")
    if value > MAX_SAFE_INTEGER:
        raise ValueError(f"{name} must not exceed {MAX_SAFE_INTEGER}")
    return value


def _rpc_id(value: object, location: str = "response id") -> RpcId:
    if isinstance(value, str):
        return value
    if isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= MAX_SAFE_INTEGER:
        return value
    raise ProtocolViolationError(
        f"{location} must be a string or non-negative JSON-safe integer"
    )


def _rpc_key(request_id: RpcId) -> _RpcKey:
    return (str if isinstance(request_id, str) else int, request_id)


def _assert_json_value(value: object, location: str = "$") -> None:
    pending: list[tuple[str, object]] = [(location, value)]
    seen: set[int] = set()
    while pending:
        current_location, current = pending.pop()
        if current is None or isinstance(current, (str, bool)):
            continue
        if isinstance(current, int):
            if abs(current) > MAX_SAFE_INTEGER:
                raise ProtocolViolationError(
                    f"{current_location} contains an unsafe JSON integer"
                )
            continue
        if isinstance(current, float):
            if not math.isfinite(current) or (
                current.is_integer() and abs(current) > MAX_SAFE_INTEGER
            ):
                raise ProtocolViolationError(
                    f"{current_location} contains an unsafe JSON number"
                )
            continue
        identity = id(current)
        if identity in seen:
            raise ProtocolViolationError(
                f"{current_location} contains a repeated or cyclic value"
            )
        seen.add(identity)
        if isinstance(current, list):
            pending.extend(
                (f"{current_location}[{index}]", child)
                for index, child in reversed(list(enumerate(current)))
            )
            continue
        if isinstance(current, dict):
            for key, child in current.items():
                if not isinstance(key, str):
                    raise ProtocolViolationError(
                        f"{current_location} contains a non-string object key"
                    )
                pending.append((f"{current_location}.{key}", child))
            continue
        raise ProtocolViolationError(
            f"{current_location} contains a non-JSON value of type {type(current).__name__}"
        )


def _object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ProtocolViolationError(f"JSON object contains duplicate field {key}")
        value[key] = item
    return value


def _reject_constant(value: str) -> None:
    raise ProtocolViolationError(f"JSON contains non-finite number {value}")


def _decode_json(frame: str) -> dict[str, Any]:
    if frame == "":
        raise ProtocolViolationError("response stream contains an empty NDJSON frame")
    try:
        value = json.loads(
            frame,
            object_pairs_hook=_object_pairs,
            parse_constant=_reject_constant,
        )
    except ProtocolViolationError:
        raise
    except (json.JSONDecodeError, UnicodeError) as error:
        raise ProtocolViolationError("response frame is not valid JSON") from error
    if not isinstance(value, dict):
        raise ProtocolViolationError("response must be a JSON-RPC object")
    _assert_json_value(value)
    return value


def _consume_background_task(task: asyncio.Future[Any]) -> None:
    if task.cancelled():
        return
    try:
        task.exception()
    except asyncio.CancelledError:
        pass


def _parse_response(value: dict[str, Any]) -> tuple[RpcId, dict[str, Any]]:
    if value.get("jsonrpc") != "2.0":
        raise ProtocolViolationError("response must use JSON-RPC 2.0")
    request_id = _rpc_id(value.get("id"))
    has_result = "result" in value
    has_error = "error" in value
    if has_result == has_error:
        raise ProtocolViolationError("response must contain exactly one of result or error")
    expected = {"jsonrpc", "id", "error" if has_error else "result"}
    if set(value) != expected:
        unknown = sorted(set(value) - expected)
        missing = sorted(expected - set(value))
        raise ProtocolViolationError(
            f"response envelope fields are invalid; unknown={unknown}, missing={missing}"
        )
    if has_error:
        error = value["error"]
        if not isinstance(error, dict) or set(error) != {"code", "message", "data"}:
            raise ProtocolViolationError("response error envelope is invalid")
        code = error.get("code")
        if (
            isinstance(code, bool)
            or not isinstance(code, int)
            or not -(2**31) <= code < 2**31
            or not isinstance(error.get("message"), str)
        ):
            raise ProtocolViolationError("response error code/message are invalid")
        data = error.get("data")
        if not isinstance(data, dict):
            raise ProtocolViolationError("response error.data must be an object")
        if not {"code", "message", "retryable"} <= set(data) <= {
            "code",
            "message",
            "retryable",
            "details",
        }:
            raise ProtocolViolationError("response error.data fields are invalid")
        if (
            not isinstance(data.get("code"), str)
            or not isinstance(data.get("message"), str)
            or not isinstance(data.get("retryable"), bool)
        ):
            raise ProtocolViolationError("response error.data values are invalid")
    return request_id, value


@dataclass(slots=True)
class _Pending:
    method: RpcMethod
    future: asyncio.Future[Any]


class DeviceRailClient(GeneratedClientMethods):
    """One asynchronous client connection to a spawned DeviceRail stdio daemon."""

    def __init__(
        self,
        process: asyncio.subprocess.Process,
        *,
        max_frame_bytes: int,
        max_pending_requests: int,
        close_grace_seconds: float,
        stderr_tail_bytes: int,
    ) -> None:
        if process.stdin is None or process.stdout is None or process.stderr is None:
            raise ValueError("DeviceRail subprocess must use piped stdin/stdout/stderr")
        self._process = process
        self._stdin = process.stdin
        self._stdout = process.stdout
        self._stderr = process.stderr
        self._decoder = NdjsonDecoder(max_frame_bytes)
        self._max_frame_bytes = max_frame_bytes
        self._max_pending_requests = max_pending_requests
        cancellation_reserve = min(16, max(1, max_pending_requests // 4))
        self._max_application_requests = max_pending_requests - cancellation_reserve
        self._close_grace_seconds = close_grace_seconds
        self._stderr_tail_bytes = stderr_tail_bytes
        self._stderr_tail = bytearray()
        self._pending: dict[_RpcKey, _Pending] = {}
        self._abandoned: dict[_RpcKey, _Pending] = {}
        self._request_prefix = str(uuid.uuid4())
        self._next_request = 1
        self._state: ClientState = "awaitingHello"
        self._enabled_features: frozenset[str] = frozenset()
        self._terminal_error: Exception | None = None
        self._write_lock = asyncio.Lock()
        self._close_task: asyncio.Task[None] | None = None
        self._reader_task = asyncio.create_task(self._read_loop(), name="devicerail-stdout")
        self._stderr_task = asyncio.create_task(self._read_stderr(), name="devicerail-stderr")

    @classmethod
    async def spawn(
        cls,
        command: str | os.PathLike[str],
        *args: str,
        hello: HelloParams | None = None,
        cwd: str | os.PathLike[str] | None = None,
        env: Mapping[str, str] | None = None,
        max_frame_bytes: int = DEFAULT_MAX_FRAME_BYTES,
        max_pending_requests: int = 256,
        close_grace_seconds: float = 7.0,
        stderr_tail_bytes: int = 64 * 1024,
    ) -> DeviceRailClient:
        """Spawn a daemon without a shell, negotiate hello, and return a ready client."""

        _positive_safe_integer(max_frame_bytes, "max_frame_bytes")
        _positive_safe_integer(max_pending_requests, "max_pending_requests")
        if max_pending_requests < 2:
            raise ValueError(
                "max_pending_requests must be at least 2 to reserve cancellation capacity"
            )
        _positive_safe_integer(stderr_tail_bytes, "stderr_tail_bytes")
        if close_grace_seconds <= 0 or not math.isfinite(close_grace_seconds):
            raise ValueError("close_grace_seconds must be finite and positive")
        process = await asyncio.create_subprocess_exec(
            os.fspath(command),
            *args,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=os.fspath(cwd) if cwd is not None else None,
            env=dict(env) if env is not None else None,
            limit=max_frame_bytes + 2,
        )
        client = cls(
            process,
            max_frame_bytes=max_frame_bytes,
            max_pending_requests=max_pending_requests,
            close_grace_seconds=close_grace_seconds,
            stderr_tail_bytes=stderr_tail_bytes,
        )
        try:
            await client.hello(default_hello() if hello is None else hello)
            return client
        except BaseException:
            await client.close()
            raise

    @property
    def state(self) -> ClientState:
        return self._state

    @property
    def enabled_features(self) -> frozenset[str]:
        return self._enabled_features

    @property
    def pending_requests(self) -> int:
        return len(self._pending)

    @property
    def stderr_tail(self) -> str:
        return bytes(self._stderr_tail).decode("utf-8", errors="replace")

    async def hello(self, params: HelloParams) -> HelloResult:
        if self._state != "awaitingHello":
            raise HandshakeStateError(
                f"system.hello is not allowed while client is {self._state}"
            )
        offer = copy.deepcopy(params)
        self._validate_hello_offer(offer)
        self._state = "helloInFlight"
        request_started = False
        try:
            handle = await self._begin_request("system.hello", offer, timeout_ms=None)
            request_started = True
            result = await handle.result
            try:
                self._validate_hello_result(result, offer)
            except ProtocolViolationError as error:
                # A schema-valid but semantically inconsistent success means the
                # peer may already consider the connection negotiated. Retrying
                # hello on that stream would create split-brain client/server
                # state, so fail the transport just like any other wire violation.
                self._fail(error)
                raise
            result_object = cast(HelloResult, result)
            self._enabled_features = frozenset(result_object["features"]["enabled"])
            if self._state == "helloInFlight":
                self._state = "ready"
            elif self._terminal_error is not None:
                raise self._terminal_error
            return result_object
        except asyncio.CancelledError:
            if request_started and self._state == "helloInFlight":
                self._fail(
                    TransportClosedError(
                        "system.hello was cancelled after the request was sent"
                    )
                )
            elif self._state == "helloInFlight":
                self._state = "awaitingHello"
            raise
        except BaseException:
            if self._state == "helloInFlight":
                self._state = "awaitingHello"
            raise

    async def cancel(self, request_id: RpcId) -> dict[str, object]:
        validated = _rpc_id(request_id, "request.cancel requestId")
        return cast(
            dict[str, object],
            await self._call("request.cancel", {"requestId": validated}, timeout_ms=None),
        )

    async def _call(
        self, method: RpcMethod, params: Any, *, timeout_ms: int | None
    ) -> Any:
        handle = await self._begin_call(method, params, timeout_ms=timeout_ms)
        return await handle.result

    async def _begin_call(
        self, method: RpcMethod, params: Any, *, timeout_ms: int | None
    ) -> RequestHandle[Any]:
        if method == "system.hello":
            raise HandshakeStateError("system.hello must be sent through hello()")
        if method == "events.subscribe":
            raise HandshakeStateError(
                "events.subscribe is available only inside the event WebSocket handshake"
            )
        if self._state != "ready":
            raise HandshakeStateError(f"{method} is not allowed while client is {self._state}")
        self._check_features(method, params, timeout_ms)
        return await self._begin_request(method, params, timeout_ms=timeout_ms)

    async def _begin_request(
        self, method: RpcMethod, params: Any, *, timeout_ms: int | None
    ) -> RequestHandle[Any]:
        if len(self._pending) >= self._max_pending_requests:
            raise PendingRequestLimitError(self._max_pending_requests)
        application = method not in ("system.hello", "request.cancel")
        application_pending = sum(
            pending.method not in ("system.hello", "request.cancel")
            for pending in self._pending.values()
        )
        if application and application_pending >= self._max_application_requests:
            raise PendingRequestLimitError(self._max_application_requests)
        request_id = f"{self._request_prefix}:{self._next_request}"
        self._next_request += 1
        request: dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
        }
        if params is not None:
            request["params"] = params
        if timeout_ms is not None:
            request["timeoutMs"] = _positive_safe_integer(timeout_ms, "timeout_ms")
        _assert_json_value(request)
        validate_document(METHOD_SPECS[method].request_schema, request)
        future: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        key = _rpc_key(request_id)
        self._pending[key] = _Pending(method, future)
        try:
            await self._write_request(request)
        except BaseException:
            pending = self._pending.pop(key, None)
            if pending is not None and not pending.future.done():
                pending.future.cancel()
            elif future.done() and not future.cancelled():
                # A fatal ambiguous-write failure may have completed every
                # pending Future before this coroutine resumes. Consume that
                # private Future because no RequestHandle will be returned.
                future.exception()
            raise
        return RequestHandle(
            request_id,
            future,
            lambda: self.cancel(request_id),
            lambda: self._cancel_waiter(request_id),
        )

    def _check_features(self, method: RpcMethod, params: Any, timeout_ms: int | None) -> None:
        required = _REQUIRED_FEATURES.get(method)
        if required is not None and required not in self._enabled_features:
            raise FeatureNotNegotiatedError(method, required)
        if (
            method == "device.execute"
            and isinstance(params, dict)
            and params.get("name") in _SEMANTIC_ACTIONS
            and SEMANTIC_ACTIONS_FEATURE not in self._enabled_features
        ):
            raise FeatureNotNegotiatedError(method, SEMANTIC_ACTIONS_FEATURE)
        uses_paged_session_export = method == "session.export" and isinstance(
            params, dict
        ) and ("afterSequence" in params or "limit" in params)
        if (
            uses_paged_session_export
            and SESSION_EXPORT_PAGE_FEATURE not in self._enabled_features
        ):
            raise FeatureNotNegotiatedError(method, SESSION_EXPORT_PAGE_FEATURE)
        spec = METHOD_SPECS[method]
        if timeout_ms is not None and not spec.timeout_supported:
            raise ProtocolViolationError(f"{method} does not support timeout_ms")
        action_timeout = (
            params.get("actionTimeoutMs")
            if method == "device.execute" and isinstance(params, dict)
            else None
        )
        if timeout_ms is not None or action_timeout is not None:
            if REQUEST_CONTROL_FEATURE not in self._enabled_features:
                raise FeatureNotNegotiatedError(method, REQUEST_CONTROL_FEATURE)

    async def _write_request(self, request: dict[str, Any]) -> None:
        try:
            serialized = json.dumps(
                request,
                ensure_ascii=False,
                allow_nan=False,
                separators=(",", ":"),
            ).encode("utf-8")
        except (TypeError, ValueError) as error:
            raise ProtocolViolationError("request is not JSON serializable") from error
        if len(serialized) > self._max_frame_bytes:
            raise WriteFrameTooLargeError(self._max_frame_bytes, len(serialized))
        delivery_started = False
        try:
            async with self._write_lock:
                if self._state in ("closing", "closed", "failed"):
                    raise self._terminal_error or TransportClosedError(
                        "DeviceRail transport is closed"
                    )
                delivery_started = True
                self._stdin.write(serialized + b"\n")
                await self._stdin.drain()
        except asyncio.CancelledError:
            if delivery_started and self._state not in ("closing", "closed", "failed"):
                # StreamWriter.write() is synchronous: once called, cancellation
                # during drain cannot prove whether the peer received the frame.
                # The connection is terminal so this request can never be
                # replayed and a late response cannot corrupt correlation state.
                self._fail(
                    TransportClosedError(
                        "request write was cancelled after delivery became ambiguous"
                    )
                )
            raise
        except (BrokenPipeError, ConnectionError) as error:
            closed = TransportClosedError("failed to write DeviceRail request")
            self._fail(closed)
            raise closed from error

    async def _read_loop(self) -> None:
        try:
            while True:
                chunk = await self._stdout.read(64 * 1024)
                if not chunk:
                    self._decoder.end()
                    break
                for frame in self._decoder.feed(chunk):
                    self._accept_response(_decode_json(frame))
            if self._state not in ("closing", "closed", "failed"):
                self._fail(
                    TransportClosedError(
                        "response stream ended before the client completed"
                    )
                )
        except asyncio.CancelledError:
            raise
        except Exception as error:
            self._fail(
                error
                if isinstance(error, ProtocolViolationError)
                else TransportClosedError(f"response stream failed: {error}")
            )

    def _accept_response(self, raw: dict[str, Any]) -> None:
        request_id, response = _parse_response(raw)
        key = _rpc_key(request_id)
        pending = self._pending.get(key)
        abandoned = self._abandoned.get(key)
        if pending is None and abandoned is None:
            raise ProtocolViolationError(
                f"response references an unknown or completed id: {request_id}"
            )
        tracked = pending if pending is not None else cast(_Pending, abandoned)
        validate_document(METHOD_SPECS[tracked.method].response_schema, response)
        if pending is None:
            self._abandoned.pop(key, None)
        else:
            self._pending.pop(key, None)
        if tracked.future.done():
            return
        if "error" in response:
            tracked.future.set_exception(
                RpcRemoteError(request_id, cast(dict[str, Any], response["error"]))
            )
        else:
            tracked.future.set_result(response["result"])

    def _detach(self, request_id: RpcId) -> RpcMethod | None:
        key = _rpc_key(request_id)
        pending = self._pending.pop(key, None)
        if pending is None:
            return None
        # A RequestHandle result is repeatable. Detaching it from the active
        # request budget must not cancel its shared Future: a later waiter can
        # still observe the peer's terminal response. The callback only marks
        # an otherwise-unobserved remote error as retrieved.
        pending.future.add_done_callback(_consume_background_task)
        if len(self._abandoned) >= self._max_pending_requests:
            error = TransportClosedError(
                "the client exceeded its bounded late-response budget"
            )
            if not pending.future.done():
                pending.future.set_exception(error)
            self._fail(error)
            return None
        self._abandoned[key] = pending
        return pending.method

    def _cancel_waiter(self, request_id: RpcId) -> None:
        method = self._detach(request_id)
        if method is None:
            return
        spec = METHOD_SPECS[method]
        if spec.timeout_supported and REQUEST_CONTROL_FEATURE in self._enabled_features:
            asyncio.create_task(
                self._cancel_abandoned(request_id), name="devicerail-request-cancel"
            )

    async def _cancel_abandoned(self, request_id: RpcId) -> None:
        try:
            await self.cancel(request_id)
        except Exception:
            pass

    def _validate_hello_offer(self, offer: HelloParams) -> None:
        request = {"jsonrpc": "2.0", "id": "hello-validation", "method": "system.hello", "params": offer}
        _assert_json_value(request)
        validate_document(METHOD_SPECS["system.hello"].request_schema, request)
        for index, protocol_range in enumerate(offer["protocol"]["ranges"]):
            if (
                protocol_range["minMinor"] > protocol_range["maxMinor"]
                or protocol_range["major"] != SUPPORTED_PROTOCOL_MAJOR
                or protocol_range["maxMinor"] > SUPPORTED_PROTOCOL_MAX_MINOR
            ):
                raise ProtocolViolationError(
                    f"system.hello protocol.ranges[{index}] exceeds client support for 1.0-1.5"
                )

    def _validate_hello_result(self, result: Any, offer: HelloParams) -> None:
        if not isinstance(result, dict):
            raise ProtocolViolationError("system.hello result must be an object")
        selected = result["protocol"]["selected"]
        if not any(
            item["major"] == selected["major"]
            and item["minMinor"] <= selected["minor"] <= item["maxMinor"]
            for item in offer["protocol"]["ranges"]
        ):
            raise ProtocolViolationError(
                "system.hello selected a protocol outside the client offer"
            )
        if (
            selected["major"] != SUPPORTED_PROTOCOL_MAJOR
            or selected["minor"] > SUPPORTED_PROTOCOL_MAX_MINOR
        ):
            raise ProtocolViolationError("system.hello selected an unsupported protocol")
        offered = set(offer.get("features", {}).get("required", [])) | set(
            offer.get("features", {}).get("optional", [])
        )
        required = set(offer.get("features", {}).get("required", []))
        enabled = set(result["features"]["enabled"])
        if not required <= enabled:
            raise ProtocolViolationError(
                f"system.hello omitted required features: {sorted(required - enabled)}"
            )
        if not enabled <= offered:
            raise ProtocolViolationError(
                f"system.hello enabled unoffered features: {sorted(enabled - offered)}"
            )
        for feature in enabled:
            minimum = _FEATURE_MINORS.get(feature)
            if minimum is not None and selected["minor"] < minimum:
                raise ProtocolViolationError(
                    f"system.hello enabled {feature} before protocol 1.{minimum}"
                )
        if result["transport"] != {"kind": "stdio", "framing": "ndjson"}:
            raise ProtocolViolationError(
                "system.hello negotiated a non-stdio/ndjson transport"
            )

    async def _read_stderr(self) -> None:
        try:
            while chunk := await self._stderr.read(16 * 1024):
                self._stderr_tail.extend(chunk)
                overflow = len(self._stderr_tail) - self._stderr_tail_bytes
                if overflow > 0:
                    del self._stderr_tail[:overflow]
        except asyncio.CancelledError:
            raise
        except Exception:
            return

    def _fail(self, error: Exception) -> None:
        if self._state in ("closed", "failed"):
            return
        self._state = "failed"
        self._terminal_error = error
        for pending in (*self._pending.values(), *self._abandoned.values()):
            if not pending.future.done():
                pending.future.set_exception(error)
        self._pending.clear()
        self._abandoned.clear()
        self._stdin.close()
        if self._process.returncode is None:
            try:
                self._process.terminate()
            except ProcessLookupError:
                pass

    async def close(self) -> None:
        if self._close_task is None:
            self._close_task = asyncio.create_task(
                self._close_impl(), name="devicerail-close"
            )
        close_task = self._close_task
        assert close_task is not None
        try:
            await asyncio.shield(close_task)
        except asyncio.CancelledError:
            # Closing owns subprocess and pipe cleanup. A caller may cancel its
            # wait, but cancellation must not cancel that shared cleanup task or
            # strand the client in `closing`. Finish cleanup before preserving
            # the caller's cancellation result.
            while not close_task.done():
                try:
                    await asyncio.shield(close_task)
                except asyncio.CancelledError:
                    # Repeated cancellation requests still cannot interrupt
                    # the owned cleanup. The original cancellation is re-raised
                    # once the close task reaches a terminal state.
                    continue
            try:
                close_task.result()
            except BaseException:
                # The cancelled caller still observes cancellation; retrieving
                # a cleanup failure here prevents an unobserved task exception.
                pass
            raise

    async def _close_impl(self) -> None:
        if self._state == "closed":
            return
        if self._state != "failed":
            self._state = "closing"
        loop = asyncio.get_running_loop()
        deadline = loop.time() + self._close_grace_seconds
        self._stdin.close()
        try:
            await asyncio.wait_for(
                self._stdin.wait_closed(),
                timeout=max(0.0, deadline - loop.time()),
            )
        except (BrokenPipeError, ConnectionError, TimeoutError):
            pass
        process_wait = asyncio.create_task(
            self._process.wait(), name="devicerail-process-wait"
        )
        try:
            await asyncio.wait_for(
                asyncio.shield(process_wait),
                timeout=max(0.0, deadline - loop.time()),
            )
        except TimeoutError:
            if self._process.returncode is None:
                try:
                    self._process.kill()
                except ProcessLookupError:
                    pass
        # Keep the uncancelled wait task alive to reap a process killed at
        # the deadline. Retrieving its result avoids an unobserved task
        # exception without extending close() beyond the configured bound.
        process_wait.add_done_callback(_consume_background_task)
        current = asyncio.current_task()
        io_tasks = [
            task
            for task in (self._reader_task, self._stderr_task)
            if task is not current
        ]
        if io_tasks:
            _, still_running = await asyncio.wait(
                io_tasks, timeout=max(0.0, deadline - loop.time())
            )
            for task in still_running:
                task.cancel()
            await asyncio.gather(*io_tasks, return_exceptions=True)
        closed = self._terminal_error or TransportClosedError(
            "DeviceRail client closed"
        )
        for pending in (*self._pending.values(), *self._abandoned.values()):
            if not pending.future.done():
                pending.future.set_exception(closed)
        self._pending.clear()
        self._abandoned.clear()
        self._state = "closed"

    async def __aenter__(self) -> DeviceRailClient:
        return self

    async def __aexit__(self, *_exc: object) -> None:
        await self.close()


__all__ = ["ClientState", "DeviceRailClient", "RpcId", "default_hello"]

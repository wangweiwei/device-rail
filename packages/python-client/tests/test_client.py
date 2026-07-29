from __future__ import annotations

import asyncio
import json
import sys
import unittest
from pathlib import Path

from devicerail import (
    DeviceRailClient,
    FeatureNotNegotiatedError,
    HandshakeStateError,
    PendingRequestLimitError,
    ProtocolViolationError,
    RpcRemoteError,
    TransportClosedError,
    default_hello,
)


MOCK_DAEMON = Path(__file__).with_name("mock_daemon.py")


class _BlockingReader:
    def __init__(self) -> None:
        self._finished = asyncio.Event()

    async def read(self, _size: int) -> bytes:
        await self._finished.wait()
        return b""

    def finish(self) -> None:
        self._finished.set()


class _ControlledStdin:
    def __init__(
        self,
        process: _ControlledProcess,
        *,
        block_drain: bool,
        finish_on_close: bool,
    ) -> None:
        self._process = process
        self._finish_on_close = finish_on_close
        self.frames: list[bytes] = []
        self.drain_started = asyncio.Event()
        self._drain_release = asyncio.Event()
        if not block_drain:
            self._drain_release.set()

    def write(self, data: bytes) -> None:
        self.frames.append(data)

    async def drain(self) -> None:
        self.drain_started.set()
        await self._drain_release.wait()

    def close(self) -> None:
        self._drain_release.set()
        if self._finish_on_close:
            self._process.finish()

    async def wait_closed(self) -> None:
        return


class _ControlledProcess:
    def __init__(
        self,
        *,
        block_drain: bool = False,
        ignore_input_close: bool = False,
        inherited_pipes: bool = False,
    ) -> None:
        self.returncode: int | None = None
        self.stdout = _BlockingReader()
        self.stderr = _BlockingReader()
        self._inherited_pipes = inherited_pipes
        self._exited = asyncio.Event()
        self.stdin = _ControlledStdin(
            self,
            block_drain=block_drain,
            finish_on_close=not ignore_input_close,
        )

    def finish(self) -> None:
        if self.returncode is not None:
            return
        self.returncode = 0
        self._exited.set()
        if not self._inherited_pipes:
            self.stdout.finish()
            self.stderr.finish()

    def terminate(self) -> None:
        self.finish()

    def kill(self) -> None:
        self.finish()

    async def wait(self) -> int:
        await self._exited.wait()
        assert self.returncode is not None
        return self.returncode


def controlled_client(
    process: _ControlledProcess, *, close_grace_seconds: float = 0.2
) -> DeviceRailClient:
    return DeviceRailClient(
        process,  # type: ignore[arg-type]
        max_frame_bytes=1024 * 1024,
        max_pending_requests=16,
        close_grace_seconds=close_grace_seconds,
        stderr_tail_bytes=4096,
    )


async def raw_mock_client(*args: str) -> DeviceRailClient:
    process = await asyncio.create_subprocess_exec(
        sys.executable,
        "-u",
        str(MOCK_DAEMON),
        *args,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    return DeviceRailClient(
        process,
        max_frame_bytes=1024 * 1024,
        max_pending_requests=256,
        close_grace_seconds=1,
        stderr_tail_bytes=4096,
    )


class DeviceRailClientTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.client = await DeviceRailClient.spawn(
            sys.executable,
            "-u",
            str(MOCK_DAEMON),
            close_grace_seconds=2,
        )

    async def asyncTearDown(self) -> None:
        await self.client.close()

    async def test_typed_calls_correlate_out_of_order_ids_and_send_timeout(self) -> None:
        first = asyncio.create_task(
            self.client.call("device.connect", timeout_ms=1_234)
        )
        second = asyncio.create_task(self.client.call("device.connect"))
        first_result, second_result = await asyncio.gather(first, second)
        self.assertTrue(first_result["connected"])
        self.assertEqual(first_result, second_result)
        capabilities = await self.client.call("device.capabilities", timeout_ms=100)
        self.assertGreater(len(capabilities), 0)
        for _ in range(50):
            if "timeoutMs=1234" in self.client.stderr_tail:
                break
            await asyncio.sleep(0.01)
        self.assertIn("timeoutMs=1234", self.client.stderr_tail)

        exported = await self.client.call(
            "session.export",
            {
                "sessionId": "33333333-3333-4333-8333-333333333333",
                "limit": 1,
            },
        )
        self.assertEqual(len(exported["events"]), 1)
        self.assertEqual(exported["nextAfterSequence"], 1)

    async def test_task_cancellation_negotiates_remote_request_cancel(self) -> None:
        observe = asyncio.create_task(
            self.client.call("device.observe", timeout_ms=10_000)
        )
        await asyncio.sleep(0.05)
        observe.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await observe
        description = await self.client.call("system.describe")
        self.assertEqual(description["connection"]["protocol"]["selected"]["minor"], 5)
        for _ in range(50):
            if self.client.pending_requests == 0:
                break
            await asyncio.sleep(0.01)
        self.assertEqual(self.client.pending_requests, 0)

    async def test_cancelling_handle_result_does_not_cancel_transport_future(self) -> None:
        handle = await self.client.begin_call("device.observe", timeout_ms=10_000)
        waiter = asyncio.ensure_future(handle.result)
        await asyncio.sleep(0.05)
        waiter.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await waiter

        description = await self.client.call("system.describe")
        self.assertEqual(description["connection"]["protocol"]["selected"]["minor"], 5)
        self.assertEqual(self.client.state, "ready")
        self.assertEqual(self.client.pending_requests, 0)

    async def test_cancelling_one_of_two_result_waiters_preserves_the_shared_result(self) -> None:
        client = await DeviceRailClient.spawn(
            sys.executable,
            "-u",
            str(MOCK_DAEMON),
            "--observe-delay-ms",
            "150",
            close_grace_seconds=2,
        )
        try:
            handle = await client.begin_call("device.observe", timeout_ms=10_000)
            cancelled_waiter = asyncio.ensure_future(handle.result)
            surviving_waiter = asyncio.ensure_future(handle.result)
            await asyncio.sleep(0.02)
            cancelled_waiter.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await cancelled_waiter
            result = await asyncio.wait_for(surviving_waiter, timeout=1)
            self.assertIn("capturedAtMs", result)
            self.assertEqual(client.pending_requests, 0)
            self.assertEqual(client.state, "ready")
        finally:
            await client.close()

    async def test_schema_and_transport_only_methods_are_rejected_before_write(self) -> None:
        with self.assertRaises(ProtocolViolationError):
            await self.client.call("device.select", {"device_id": "mock-device-1"})
        with self.assertRaises(ProtocolViolationError):
            await self.client.call("session.start", timeout_ms=1)  # type: ignore[call-overload]
        with self.assertRaises(HandshakeStateError):
            await self.client.call(
                "events.subscribe",
                {"sessionId": "33333333-3333-4333-8333-333333333333"},
            )
        with self.assertRaises(ProtocolViolationError):
            await self.client.call(
                "device.execute",
                {
                    "id": "00000000-0000-4000-8000-000000000000",
                    "name": "tap",
                    "arguments": 9_007_199_254_740_992.0,
                },
            )

    async def test_pending_limit_reserves_capacity_for_explicit_cancel(self) -> None:
        client = await DeviceRailClient.spawn(
            sys.executable,
            "-u",
            str(MOCK_DAEMON),
            max_pending_requests=2,
            close_grace_seconds=2,
        )
        try:
            handle = await client.begin_call("device.observe", timeout_ms=10_000)
            with self.assertRaises(PendingRequestLimitError):
                await client.begin_call("device.observe", timeout_ms=10_000)
            cancellation = await handle.cancel()
            self.assertEqual(cancellation["status"], "requested")
            with self.assertRaises(RpcRemoteError):
                await handle.result
        finally:
            await client.close()


class MalformedDaemonTests(unittest.IsolatedAsyncioTestCase):
    async def test_malformed_or_semantically_invalid_hello_is_terminal(self) -> None:
        for mode in (
            "duplicate",
            "media-before-version",
            "semantic-hello",
            "unknown-field",
        ):
            with self.subTest(mode=mode):
                with self.assertRaises(ProtocolViolationError):
                    await DeviceRailClient.spawn(
                        sys.executable,
                        "-u",
                        str(MOCK_DAEMON),
                        "--malformed",
                        mode,
                        close_grace_seconds=2,
                    )

    async def test_cancelled_hello_is_terminal_but_remote_rejection_can_retry(self) -> None:
        cancelled_client = await raw_mock_client("--hello-delay-ms", "250")
        try:
            hello = asyncio.create_task(cancelled_client.hello(default_hello()))
            await asyncio.sleep(0.05)
            hello.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await hello
            self.assertEqual(cancelled_client.state, "failed")
            with self.assertRaises(HandshakeStateError):
                await cancelled_client.hello(default_hello())
        finally:
            await cancelled_client.close()

        retry_client = await raw_mock_client("--malformed", "remote-hello-once")
        try:
            with self.assertRaises(RpcRemoteError):
                await retry_client.hello(default_hello())
            self.assertEqual(retry_client.state, "awaitingHello")
            result = await retry_client.hello(default_hello())
            self.assertEqual(result["protocol"]["selected"]["minor"], 5)
            self.assertEqual(retry_client.state, "ready")
        finally:
            await retry_client.close()

    async def test_stderr_tail_is_explicit_and_never_in_transport_exception(self) -> None:
        client = await raw_mock_client("--malformed", "stderr-eof")
        try:
            with self.assertRaises(TransportClosedError) as raised:
                await client.hello(default_hello())
            self.assertNotIn("PYTHON-CLIENT-SECRET", str(raised.exception))
            for _ in range(50):
                if "PYTHON-CLIENT-SECRET" in client.stderr_tail:
                    break
                await asyncio.sleep(0.01)
            self.assertIn("PYTHON-CLIENT-SECRET", client.stderr_tail)
        finally:
            await client.close()


class ControlledTransportTests(unittest.IsolatedAsyncioTestCase):
    async def test_protocol_15_methods_and_semantic_actions_require_features_before_write(
        self,
    ) -> None:
        process = _ControlledProcess()
        client = controlled_client(process)
        client._state = "ready"  # type: ignore[assignment]
        client._enabled_features = frozenset()
        calls = (
            (
                "ui.snapshot.get",
                {"observationId": "11111111-1111-4111-8111-111111111111"},
                "observation.uiSnapshot.v1",
            ),
            (
                "verdict.record",
                {
                    "verdict": {
                        "status": "unknown",
                        "summary": "insufficient evidence",
                        "evidence": [],
                    }
                },
                "verdict.record.v1",
            ),
            (
                "device.execute",
                {
                    "id": "22222222-2222-4222-8222-222222222222",
                    "name": "findElement",
                    "arguments": {"selector": {"role": "button"}},
                },
                "device.semanticActions.v1",
            ),
        )
        for method, params, feature in calls:
            with self.subTest(method=method):
                with self.assertRaises(FeatureNotNegotiatedError) as raised:
                    await client.begin_call(method, params)  # type: ignore[arg-type]
                self.assertEqual(raised.exception.feature, feature)
        self.assertFalse(process.stdin.frames)
        await client.close()

    async def test_media_methods_require_feature_before_any_write(self) -> None:
        process = _ControlledProcess()
        client = controlled_client(process)
        client._state = "ready"  # type: ignore[assignment]
        client._enabled_features = frozenset()
        stream_id = "77777777-7777-4777-8777-777777777777"
        calls = (
            ("media.stream.start", {"streamId": stream_id, "kind": "screenshot"}),
            (
                "media.stream.capture",
                {"streamId": stream_id, "frameIndex": 1},
            ),
            ("media.stream.end", {"streamId": stream_id}),
        )
        for method, params in calls:
            with self.subTest(method=method):
                with self.assertRaises(FeatureNotNegotiatedError) as raised:
                    await client.begin_call(method, params)  # type: ignore[arg-type]
                self.assertEqual(raised.exception.feature, "media.stream.v1")
        self.assertFalse(process.stdin.frames)
        await client.close()

    async def test_media_capture_accepts_request_timeout(self) -> None:
        process = _ControlledProcess()
        client = controlled_client(process)
        client._state = "ready"  # type: ignore[assignment]
        client._enabled_features = frozenset(
            {"media.stream.v1", "request.control.v1"}
        )
        stream_id = "77777777-7777-4777-8777-777777777777"
        handle = await client.begin_call(
            "media.stream.capture",
            {"streamId": stream_id, "frameIndex": 1, "durationMs": 100},
            timeout_ms=1_234,
        )
        self.assertEqual(len(process.stdin.frames), 1)
        request = json.loads(process.stdin.frames[0])
        self.assertEqual(request["method"], "media.stream.capture")
        self.assertEqual(request["timeoutMs"], 1_234)
        self.assertEqual(request["params"]["frameIndex"], 1)
        await client.close()
        with self.assertRaises(TransportClosedError):
            await handle.result

    async def test_session_export_pagination_requires_its_additive_feature(self) -> None:
        process = _ControlledProcess()
        client = controlled_client(process)
        client._state = "ready"  # type: ignore[assignment]
        client._enabled_features = frozenset({"events.snapshot.v1"})
        with self.assertRaises(FeatureNotNegotiatedError) as raised:
            await client.begin_call(
                "session.export",
                {
                    "sessionId": "33333333-3333-4333-8333-333333333333",
                    "limit": 1,
                },
            )
        self.assertEqual(raised.exception.feature, "session.export.page.v1")
        self.assertFalse(process.stdin.frames)
        await client.close()

    async def test_cancellation_after_write_started_poisoned_ambiguous_transport(self) -> None:
        process = _ControlledProcess(block_drain=True)
        client = controlled_client(process)
        client._state = "ready"  # type: ignore[assignment]
        client._enabled_features = frozenset({"request.control.v1"})
        request = asyncio.create_task(client.begin_call("device.observe", timeout_ms=10_000))
        await asyncio.wait_for(process.stdin.drain_started.wait(), timeout=1)
        request.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await request
        self.assertTrue(process.stdin.frames)
        self.assertEqual(client.state, "failed")
        self.assertEqual(client.pending_requests, 0)
        with self.assertRaises(HandshakeStateError):
            await client.call("system.describe")
        await client.close()

    async def test_close_deadline_cancels_inherited_pipe_readers(self) -> None:
        process = _ControlledProcess(inherited_pipes=True)
        client = controlled_client(process, close_grace_seconds=0.05)
        started = asyncio.get_running_loop().time()
        await asyncio.wait_for(client.close(), timeout=0.5)
        elapsed = asyncio.get_running_loop().time() - started
        self.assertLess(elapsed, 0.3)
        self.assertEqual(client.state, "closed")
        self.assertTrue(client._reader_task.done())
        self.assertTrue(client._stderr_task.done())

    async def test_cancelling_close_waits_for_owned_cleanup_before_propagating(self) -> None:
        process = _ControlledProcess(ignore_input_close=True)
        client = controlled_client(process, close_grace_seconds=0.05)
        close = asyncio.create_task(client.close())
        await asyncio.sleep(0.01)
        close.cancel()
        await asyncio.sleep(0.01)
        close.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(close, timeout=0.5)
        self.assertEqual(client.state, "closed")
        self.assertEqual(process.returncode, 0)
        self.assertTrue(client._reader_task.done())
        self.assertTrue(client._stderr_task.done())
        await client.close()


if __name__ == "__main__":
    unittest.main()

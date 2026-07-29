from __future__ import annotations

import asyncio
import gc
import unittest
from collections.abc import Awaitable, Callable
from typing import Any

from devicerail import RequestHandle, TransportClosedError


async def _unused_remote_cancel() -> dict[str, object]:
    return {"status": "requested"}


class RequestResultCancellationTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.loop = asyncio.get_running_loop()
        self.loop_contexts: list[dict[str, Any]] = []
        self.previous_exception_handler = self.loop.get_exception_handler()
        self.loop.set_exception_handler(
            lambda _loop, context: self.loop_contexts.append(context)
        )

    async def asyncTearDown(self) -> None:
        try:
            await asyncio.sleep(0)
            gc.collect()
            await asyncio.sleep(0)
            self.assertEqual(self.loop_contexts, [])
        finally:
            self.loop.set_exception_handler(self.previous_exception_handler)

    def handle(
        self,
        future: asyncio.Future[int],
        on_last_waiter_cancelled: Callable[[], None],
    ) -> RequestHandle[int]:
        cancel: Callable[[], Awaitable[dict[str, object]]] = _unused_remote_cancel
        return RequestHandle("request-1", future, cancel, on_last_waiter_cancelled)

    async def test_cancel_before_first_task_turn_cannot_leak_a_terminal_error(self) -> None:
        future: asyncio.Future[int] = self.loop.create_future()
        detached: list[None] = []
        handle = self.handle(future, lambda: detached.append(None))
        waiter = asyncio.ensure_future(handle.result)

        # A generic Awaitable is wrapped in an asyncio Task. Cancelling that
        # Task before its first turn means _wait() never starts and therefore
        # cannot run its cancellation finally block.
        waiter.cancel()
        future.set_exception(RuntimeError("remote failure after immediate cancel"))
        with self.assertRaises(asyncio.CancelledError):
            await waiter
        self.assertEqual(detached, [])

    async def test_terminal_error_winning_last_waiter_cancel_is_consumed(self) -> None:
        future: asyncio.Future[int] = self.loop.create_future()
        detached: list[None] = []
        handle = self.handle(future, lambda: detached.append(None))
        waiter = asyncio.ensure_future(handle.result)
        await asyncio.sleep(0)

        waiter.cancel()
        future.set_exception(RuntimeError("remote failure in cancellation race"))
        with self.assertRaises(asyncio.CancelledError):
            await waiter
        self.assertEqual(detached, [])

    async def test_one_cancelled_waiter_preserves_shared_error_for_survivor(self) -> None:
        future: asyncio.Future[int] = self.loop.create_future()
        detached: list[None] = []
        handle = self.handle(future, lambda: detached.append(None))
        cancelled = asyncio.ensure_future(handle.result)
        survivor = asyncio.ensure_future(handle.result)
        await asyncio.sleep(0)

        cancelled.cancel()
        failure = RuntimeError("shared remote failure")
        future.set_exception(failure)
        with self.assertRaises(asyncio.CancelledError):
            await cancelled
        with self.assertRaises(RuntimeError) as raised:
            await survivor
        self.assertIs(raised.exception, failure)
        self.assertEqual(detached, [])

    async def test_cancelling_all_waiters_detaches_once_and_consumes_late_error(self) -> None:
        future: asyncio.Future[int] = self.loop.create_future()
        detached: list[None] = []
        handle = self.handle(future, lambda: detached.append(None))
        first = asyncio.ensure_future(handle.result)
        second = asyncio.ensure_future(handle.result)
        await asyncio.sleep(0)

        first.cancel()
        second.cancel()
        for waiter in (first, second):
            with self.assertRaises(asyncio.CancelledError):
                await waiter
        self.assertEqual(detached, [None])
        future.set_exception(RuntimeError("late abandoned response"))

    async def test_close_error_remains_repeatable_after_internal_consumption(self) -> None:
        future: asyncio.Future[int] = self.loop.create_future()
        detached: list[None] = []
        handle = self.handle(future, lambda: detached.append(None))
        closed = TransportClosedError("client closed with a pending request")
        future.set_exception(closed)
        await asyncio.sleep(0)

        for _ in range(2):
            with self.assertRaises(TransportClosedError) as raised:
                await handle.result
            self.assertIs(raised.exception, closed)
        self.assertEqual(detached, [])

    async def test_shared_future_cancellation_is_not_mistaken_for_waiter_cancel(self) -> None:
        future: asyncio.Future[int] = self.loop.create_future()
        detached: list[None] = []
        handle = self.handle(future, lambda: detached.append(None))
        waiter = asyncio.ensure_future(handle.result)
        await asyncio.sleep(0)

        future.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await waiter
        with self.assertRaises(asyncio.CancelledError):
            await handle.result
        self.assertEqual(detached, [])


if __name__ == "__main__":
    unittest.main()

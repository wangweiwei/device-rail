"""Small runtime types shared by generated overloads and the client."""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable, Generator
from typing import Any, Generic, TypeVar


ResultT = TypeVar("ResultT", covariant=True)


def _consume_terminal_exception(future: asyncio.Future[Any]) -> None:
    """Mark an internal terminal error retrieved without changing its result."""

    if not future.cancelled():
        future.exception()


class _CancellationIsolatedRequestResult(Generic[ResultT]):
    """A repeatable awaitable that never exposes the transport's Future.

    Cancelling a task that awaits this object cancels only that waiter. The
    client callback atomically accounts for the still-in-flight wire request.
    """

    __slots__ = ("_active_waiters", "_future", "_on_last_waiter_cancelled")

    def __init__(
        self,
        future: asyncio.Future[ResultT],
        on_last_waiter_cancelled: Callable[[], None],
    ) -> None:
        self._active_waiters = 0
        self._future = future
        self._on_last_waiter_cancelled = on_last_waiter_cancelled
        # The shared transport Future is an implementation detail, not a Task
        # the caller can retrieve directly. Always mark its terminal error as
        # observed: Future.exception() does not clear the error, so every
        # current or later waiter still receives it from Future.result(). This
        # also covers a generic Awaitable wrapper cancelled before its coroutine
        # gets its first turn and therefore before _wait() can enter its finally
        # block.
        future.add_done_callback(_consume_terminal_exception)

    async def _wait(self) -> ResultT:
        self._active_waiters += 1
        waiter_cancelled = False
        try:
            # asyncio.wait() observes completion without forwarding waiter
            # cancellation into the shared transport Future.  Using
            # asyncio.shield() here would make Python 3.14 deliberately report
            # a remote error through the event-loop exception handler after a
            # shielded waiter is cancelled, even when DeviceRail has installed
            # its own bounded late-response consumer.
            await asyncio.wait((self._future,), return_when=asyncio.ALL_COMPLETED)
            return self._future.result()
        except asyncio.CancelledError:
            waiter_cancelled = True
            raise
        finally:
            self._active_waiters -= 1
            if (
                waiter_cancelled
                and self._active_waiters == 0
                and not self._future.done()
            ):
                self._on_last_waiter_cancelled()

    def __await__(self) -> Generator[Any, None, ResultT]:
        return self._wait().__await__()


class RequestHandle(Generic[ResultT]):
    """A written RPC request whose result can be awaited or cancelled remotely."""

    def __init__(
        self,
        request_id: str | int,
        result: asyncio.Future[ResultT],
        cancel: Callable[[], Awaitable[dict[str, object]]],
        on_waiter_cancelled: Callable[[], None],
    ) -> None:
        self.id = request_id
        self.result: Awaitable[ResultT] = _CancellationIsolatedRequestResult(
            result, on_waiter_cancelled
        )
        self._cancel = cancel

    async def cancel(self) -> dict[str, object]:
        return await self._cancel()


__all__ = ["RequestHandle"]

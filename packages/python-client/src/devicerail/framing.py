"""Strict, byte-bounded NDJSON framing."""

from __future__ import annotations

from .errors import (
    NdjsonFrameTooLargeError,
    NdjsonIncompleteFrameError,
    NdjsonInvalidUtf8Error,
    TransportClosedError,
)


DEFAULT_MAX_FRAME_BYTES = 1024 * 1024


class NdjsonDecoder:
    """Incrementally decode LF/CRLF NDJSON with a byte limit and fatal UTF-8."""

    def __init__(self, max_frame_bytes: int = DEFAULT_MAX_FRAME_BYTES) -> None:
        if isinstance(max_frame_bytes, bool) or max_frame_bytes <= 0:
            raise ValueError("max_frame_bytes must be a positive integer")
        self.max_frame_bytes = max_frame_bytes
        self._buffer = bytearray()
        self._ended = False
        self._failure: Exception | None = None

    @property
    def buffered_bytes(self) -> int:
        return len(self._buffer)

    def feed(self, chunk: bytes) -> list[str]:
        self._assert_open()
        if not isinstance(chunk, bytes):
            raise TypeError("NDJSON chunks must be bytes")
        self._buffer.extend(chunk)
        frames: list[str] = []
        consumed = 0
        while True:
            newline = self._buffer.find(b"\n", consumed)
            if newline < 0:
                break
            raw = bytes(self._buffer[consumed:newline])
            consumed = newline + 1
            frame = raw[:-1] if raw.endswith(b"\r") else raw
            self._check_bound(len(frame))
            try:
                frames.append(frame.decode("utf-8", errors="strict"))
            except UnicodeDecodeError as error:
                self._fail(NdjsonInvalidUtf8Error("NDJSON frame is not valid UTF-8"))
                raise AssertionError("unreachable") from error
        if consumed:
            # Compact once per feed call. Deleting every decoded prefix shifts
            # the remaining bytearray repeatedly and becomes quadratic when a
            # single transport read contains many small frames.
            del self._buffer[:consumed]
        pending = len(self._buffer)
        may_end_in_cr = pending == self.max_frame_bytes + 1 and self._buffer[-1:] == b"\r"
        if pending > self.max_frame_bytes and not may_end_in_cr:
            self._fail(NdjsonFrameTooLargeError(self.max_frame_bytes, pending))
        return frames

    def end(self) -> None:
        self._assert_open()
        self._ended = True
        if self._buffer:
            self._fail(NdjsonIncompleteFrameError(len(self._buffer)))

    def _check_bound(self, actual_bytes: int) -> None:
        if actual_bytes > self.max_frame_bytes:
            self._fail(NdjsonFrameTooLargeError(self.max_frame_bytes, actual_bytes))

    def _assert_open(self) -> None:
        if self._failure is not None:
            raise self._failure
        if self._ended:
            raise TransportClosedError("NDJSON decoder has already ended")

    def _fail(self, error: Exception) -> None:
        self._failure = error
        raise error


__all__ = ["DEFAULT_MAX_FRAME_BYTES", "NdjsonDecoder"]

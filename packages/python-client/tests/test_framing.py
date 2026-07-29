from __future__ import annotations

import unittest

from devicerail import (
    NdjsonFrameTooLargeError,
    NdjsonIncompleteFrameError,
    NdjsonInvalidUtf8Error,
)
from devicerail.framing import NdjsonDecoder


class NdjsonDecoderTests(unittest.TestCase):
    def test_fragmented_lf_and_crlf_frames_are_byte_bounded(self) -> None:
        decoder = NdjsonDecoder(5)
        self.assertEqual(decoder.feed(b"hello\r"), [])
        self.assertEqual(decoder.feed(b"\nwor"), ["hello"])
        self.assertEqual(decoder.feed(b"ld\n"), ["world"])
        decoder.end()

    def test_oversized_invalid_utf8_and_incomplete_frames_fail(self) -> None:
        with self.assertRaises(NdjsonFrameTooLargeError):
            NdjsonDecoder(2).feed(b"abc")
        with self.assertRaises(NdjsonInvalidUtf8Error):
            NdjsonDecoder().feed(b"\xff\n")
        decoder = NdjsonDecoder()
        decoder.feed(b"partial")
        with self.assertRaises(NdjsonIncompleteFrameError):
            decoder.end()

    def test_many_frames_in_one_chunk_compact_once_and_preserve_order(self) -> None:
        decoder = NdjsonDecoder(8)
        expected = [str(index) for index in range(10_000)]
        payload = b"".join(f"{value}\n".encode("ascii") for value in expected)
        self.assertEqual(decoder.feed(payload), expected)
        self.assertEqual(decoder.buffered_bytes, 0)
        decoder.end()


if __name__ == "__main__":
    unittest.main()

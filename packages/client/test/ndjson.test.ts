import assert from "node:assert/strict";
import test from "node:test";

import {
  NdjsonDecoder,
  NdjsonFrameTooLargeError,
  NdjsonIncompleteFrameError,
  NdjsonInvalidUtf8Error,
} from "../src/index.js";

test("decoder handles split chunks, multiple frames, and CRLF", () => {
  const decoder = new NdjsonDecoder({ maxFrameBytes: 64 });

  assert.deepEqual(decoder.push(Buffer.from('{"id":1')),
    []);
  assert.deepEqual(
    decoder.push(Buffer.from('}\r\n{"id":2}\n{"id":')),
    ['{"id":1}', '{"id":2}'],
  );
  assert.deepEqual(decoder.push(Buffer.from("3}\n")), ['{"id":3}']);
  decoder.end();
});

test("decoder measures frame limits in bytes and permits a terminal CR", () => {
  const exact = new NdjsonDecoder({ maxFrameBytes: 4 });
  assert.deepEqual(exact.push(Buffer.from("éé\r\n")), ["éé"]);
  exact.end();

  const oversized = new NdjsonDecoder({ maxFrameBytes: 3 });
  assert.throws(() => oversized.push(Buffer.from("éé\n")), NdjsonFrameTooLargeError);

  const pending = new NdjsonDecoder({ maxFrameBytes: 3 });
  assert.throws(() => pending.push(Buffer.from("1234")), NdjsonFrameTooLargeError);
});

test("decoder rejects malformed UTF-8 instead of replacing bytes", () => {
  const decoder = new NdjsonDecoder();
  assert.throws(
    () => decoder.push(Uint8Array.from([0xc3, 0x28, 0x0a])),
    NdjsonInvalidUtf8Error,
  );
});

test("decoder rejects an EOF residual frame", () => {
  const decoder = new NdjsonDecoder();
  decoder.push(Buffer.from('{"partial":true}'));
  assert.throws(() => decoder.end(), NdjsonIncompleteFrameError);
});
